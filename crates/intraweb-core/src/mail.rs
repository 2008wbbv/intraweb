//! Local mail: a signed note from one key to another.
//!
//! Mail is addressed to a public key, never to a nickname. You may *type* a
//! nickname, and the CLI resolves it, but what gets signed, sent, and filed is
//! the key -- so a delivered message still means something a week later when
//! two people are using the same label.
//!
//! Delivery is the sender's problem. A message sits in the outbox until the
//! recipient's node is reachable, then goes straight there. No hub ever holds
//! it, which is also why there is no such thing as mail to someone who has
//! never been on the network.

use crate::identity::{Identity, PeerId, SIG_LEN};
use anyhow::{Context, Result, ensure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD as B64;
use serde::{Deserialize, Serialize};

/// Bumped when the signed encoding changes.
pub const MAIL_VERSION: u16 = 1;

/// Domain separation, so a mail signature can never be replayed as a presence
/// record or anything else we sign later.
const SIGNING_DOMAIN: &str = "intraweb-mail-v1";

/// Caps, because this is parsed from bytes any stranger on the LAN can send.
pub const MAX_SUBJECT_BYTES: usize = 200;
pub const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    pub version: u16,
    pub from: PeerId,
    pub to: PeerId,
    pub subject: String,
    pub body: String,
    pub created_at: u64,
}

impl Mail {
    pub fn new(from: PeerId, to: PeerId, subject: &str, body: &str, created_at: u64) -> Self {
        Self {
            version: MAIL_VERSION,
            from,
            to,
            subject: subject.to_string(),
            body: body.to_string(),
            created_at,
        }
    }

    /// Deterministic bytes covered by the signature.
    ///
    /// Subject and body are base64'd rather than scrubbed. A nickname is a
    /// label and can afford to lose characters; a message cannot, and it has to
    /// be free to contain the separator. Encoding removes the ambiguity instead
    /// of removing the user's text.
    pub fn signing_bytes(&self) -> Vec<u8> {
        format!(
            "{SIGNING_DOMAIN}|{}|{}|{}|{}|{}|{}",
            self.version,
            self.from.to_hex(),
            self.to.to_hex(),
            self.created_at,
            B64.encode(self.subject.as_bytes()),
            B64.encode(self.body.as_bytes()),
        )
        .into_bytes()
    }

    pub fn sign(&self, identity: &Identity) -> [u8; SIG_LEN] {
        identity.sign(&self.signing_bytes())
    }

    /// Check size limits. Called before signing and again on receipt.
    pub fn check_limits(&self) -> Result<()> {
        ensure!(
            self.subject.len() <= MAX_SUBJECT_BYTES,
            "subject is {} bytes; the limit is {MAX_SUBJECT_BYTES}",
            self.subject.len(),
        );
        ensure!(
            self.body.len() <= MAX_BODY_BYTES,
            "message is {} bytes; the limit is {MAX_BODY_BYTES}",
            self.body.len(),
        );
        Ok(())
    }

    /// Verify that this really came from the key it names.
    pub fn verify(&self, signature: &[u8]) -> Result<()> {
        ensure!(
            self.version == MAIL_VERSION,
            "unsupported mail version {}",
            self.version
        );
        self.check_limits()?;
        self.from.verify(&self.signing_bytes(), signature)
    }
}

/// A mail envelope as it travels over the wire, as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedMail {
    #[serde(flatten)]
    pub mail: Mail,
    /// Base64, so the envelope stays plain JSON.
    pub signature: String,
}

impl SignedMail {
    pub fn seal(mail: Mail, identity: &Identity) -> Result<Self> {
        mail.check_limits()?;
        let signature = B64.encode(mail.sign(identity));
        Ok(Self { mail, signature })
    }

    pub fn signature_bytes(&self) -> Result<Vec<u8>> {
        B64.decode(&self.signature)
            .context("signature is not valid base64")
    }

    /// Verify the envelope and confirm it is addressed to us.
    ///
    /// Both halves matter: the signature proves who wrote it, and the
    /// recipient check stops a node being used to store other people's mail.
    pub fn accept(&self, me: PeerId) -> Result<()> {
        ensure!(
            self.mail.to == me,
            "this message is addressed to somebody else"
        );
        let signature = self.signature_bytes()?;
        self.mail.verify(&signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Identity, Identity) {
        (Identity::generate().unwrap(), Identity::generate().unwrap())
    }

    #[test]
    fn a_sealed_letter_is_accepted_by_its_recipient() {
        let (alice, bob) = pair();
        let mail = Mail::new(
            alice.peer_id(),
            bob.peer_id(),
            "fuel drop",
            "thursday, 0600",
            100,
        );

        let sealed = SignedMail::seal(mail, &alice).unwrap();
        sealed.accept(bob.peer_id()).unwrap();
    }

    #[test]
    fn a_letter_for_somebody_else_is_refused() {
        let (alice, bob) = pair();
        let carol = Identity::generate().unwrap();
        let sealed = SignedMail::seal(
            Mail::new(alice.peer_id(), bob.peer_id(), "s", "b", 100),
            &alice,
        )
        .unwrap();

        // Carol must not become a store for mail that is not hers.
        assert!(sealed.accept(carol.peer_id()).is_err());
    }

    #[test]
    fn a_forged_sender_does_not_verify() {
        let (alice, bob) = pair();
        let mallory = Identity::generate().unwrap();

        // Mallory writes a letter claiming to be Alice, signing with their own key.
        let mail = Mail::new(
            alice.peer_id(),
            bob.peer_id(),
            "lend me the truck",
            "-- alice",
            100,
        );
        let sealed = SignedMail {
            signature: base64_of(mail.sign(&mallory)),
            mail,
        };

        assert!(sealed.accept(bob.peer_id()).is_err());
    }

    #[test]
    fn tampering_with_the_body_breaks_the_signature() {
        let (alice, bob) = pair();
        let mut sealed = SignedMail::seal(
            Mail::new(alice.peer_id(), bob.peer_id(), "s", "meet at six", 100),
            &alice,
        )
        .unwrap();

        sealed.mail.body = "meet at nine".into();
        assert!(sealed.accept(bob.peer_id()).is_err());
    }

    #[test]
    fn separators_in_a_message_cannot_forge_the_envelope() {
        let (alice, bob) = pair();
        // A body crafted to look like extra signed fields.
        let hostile = "|999|deadbeef|other subject|";
        let sealed = SignedMail::seal(
            Mail::new(alice.peer_id(), bob.peer_id(), "s", hostile, 100),
            &alice,
        )
        .unwrap();

        sealed.accept(bob.peer_id()).unwrap();
        assert_eq!(sealed.mail.body, hostile, "the text survives intact");
    }

    #[test]
    fn oversized_messages_are_refused_before_they_are_signed() {
        let (alice, bob) = pair();
        let huge = "x".repeat(MAX_BODY_BYTES + 1);
        let mail = Mail::new(alice.peer_id(), bob.peer_id(), "s", &huge, 100);

        assert!(SignedMail::seal(mail, &alice).is_err());
    }

    #[test]
    fn the_envelope_round_trips_as_json() {
        let (alice, bob) = pair();
        let sealed = SignedMail::seal(
            Mail::new(alice.peer_id(), bob.peer_id(), "hi", "there", 100),
            &alice,
        )
        .unwrap();

        let wire = serde_json::to_string(&sealed).unwrap();
        let parsed: SignedMail = serde_json::from_str(&wire).unwrap();

        parsed.accept(bob.peer_id()).unwrap();
        assert_eq!(parsed.mail, sealed.mail);
    }

    fn base64_of(sig: [u8; SIG_LEN]) -> String {
        B64.encode(sig)
    }
}
