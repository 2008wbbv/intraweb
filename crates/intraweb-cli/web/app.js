"use strict";

// The dashboard is a thin client. Every fact it shows comes from the local
// JSON API, which the terminal UI reads too, so the two can never disagree.

const REFRESH_MS = 2000;

/** Nicknames are scrubbed server-side, but never build markup from remote text. */
function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  })[c]);
}

function relativeTime(seconds) {
  if (seconds < 2) return "just now";
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  return `${Math.floor(minutes / 60)}h ago`;
}

async function getJson(path) {
  const response = await fetch(path, { cache: "no-store" });
  if (!response.ok) throw new Error(`${path} responded ${response.status}`);
  return response.json();
}

function trustTag(peer) {
  switch (peer.trust) {
    case "verified":
      return '<span class="tag verified">verified</span>';
    case "nickname_conflict":
      return '<span class="tag conflict">name clash</span>';
    case "new":
      return '<span class="tag">new</span>';
    default:
      return "";
  }
}

function peerCard(peer, now) {
  const alarming = peer.trust === "nickname_conflict";
  const classes = ["card"];
  if (peer.is_hub) classes.push("is-hub");
  if (alarming) classes.push("is-alarming");

  const address = (peer.addrs && peer.addrs[0]) || null;
  const siteUrl = address
    ? `http://${address.includes(":") ? `[${address}]` : address}:${peer.api_port}/~${encodeURIComponent(peer.nickname)}`
    : null;

  // A name clash is the one thing worth interrupting someone over: it means a
  // familiar label arrived carrying a key we have never seen before.
  const alarm = alarming
    ? `<div class="alarm"><strong>This is not the ${escapeHtml(peer.nickname)} you met before.</strong>
        Same name, different key. Compare fingerprints in person before trusting it.</div>`
    : "";

  return `
    <article class="${classes.join(" ")}">
      <div class="name">
        ${escapeHtml(peer.nickname)}
        ${peer.is_hub ? '<span class="tag hub">hub</span>' : ""}
        ${trustTag(peer)}
      </div>
      <div class="fp">${escapeHtml(peer.fingerprint)}</div>
      <div class="meta">
        ${address ? escapeHtml(address) : "address unknown"}
        &middot; via ${escapeHtml(peer.source)}
        &middot; seen ${escapeHtml(relativeTime(Math.max(0, now - peer.last_seen)))}
      </div>
      ${alarm}
      <div class="actions">
        ${siteUrl ? `<a href="${siteUrl}" target="_blank" rel="noopener">Visit site</a>` : ""}
        ${peer.trust !== "verified"
          ? `<button type="button" data-verify="${escapeHtml(peer.peer_id)}">I checked the fingerprint</button>`
          : ""}
      </div>
    </article>`;
}

function renderMe(status) {
  document.getElementById("me").innerHTML = `
    <div class="nick">${escapeHtml(status.nickname)}${
      status.is_hub ? ` <span class="badge-hub">hosting ${escapeHtml(status.hub_name || "")}</span>` : ""
    }</div>
    <div class="fp">${escapeHtml(status.fingerprint)}</div>`;
  document.getElementById("vault-path").textContent = `vault: ${status.vault_path}`;
}

function renderRosters(peers, status) {
  const now = status.now;
  const hubs = peers.filter((p) => p.is_hub);
  const neighbors = peers.filter((p) => !p.is_hub);

  document.getElementById("hub-count").textContent =
    hubs.length === 0 ? "" : `${hubs.length} in range`;
  document.getElementById("peer-count").textContent =
    neighbors.length === 0 ? "" : `${neighbors.length} online`;

  document.getElementById("hubs").innerHTML = hubs.length
    ? hubs.map((p) => peerCard(p, now)).join("")
    : `<p class="empty">No hubs in range. ${
        status.is_hub ? "You are hosting one yourself." : "Start one with <code>intraweb up --hub</code>."
      }</p>`;

  document.getElementById("peers").innerHTML = neighbors.length
    ? neighbors.map((p) => peerCard(p, now)).join("")
    : '<p class="empty">Nobody else yet. Run the network check if you expected company.</p>';
}

function renderDoctor(report) {
  const checks = report.checks
    .map(
      (check) => `
      <div class="check">
        <span class="dot ${escapeHtml(check.status)}"></span>
        <span>
          <span class="label">${escapeHtml(check.name)}</span> &mdash; ${escapeHtml(check.detail)}
          ${check.remedy ? `<div class="remedy">${escapeHtml(check.remedy)}</div>` : ""}
        </span>
      </div>`,
    )
    .join("");
  document.getElementById("doctor").innerHTML =
    `${checks}<div class="verdict">${escapeHtml(report.verdict)}</div>`;
}

function renderMail(messages, now) {
  const unread = messages.filter((m) => !m.read_at).length;
  document.getElementById("mail-count").textContent =
    messages.length === 0 ? "" : `${messages.length} received${unread ? `, ${unread} unread` : ""}`;

  document.getElementById("mail").innerHTML = messages.length
    ? messages
        .map(
          (message) => `
        <article class="letter ${message.read_at ? "" : "unread"}">
          <div class="subject">${escapeHtml(message.subject || "(no subject)")}</div>
          <div class="from">${escapeHtml(message.peer_id.slice(0, 16))}&hellip;
            &middot; ${escapeHtml(relativeTime(Math.max(0, now - message.created_at)))}</div>
          <div class="body">${escapeHtml(message.body)}</div>
        </article>`,
        )
        .join("")
    : '<p class="empty">No mail yet.</p>';
}

async function refresh() {
  try {
    // One request: status, roster and mail are always rendered together.
    const state = await getJson("/api/state");
    renderMe(state.status);
    renderRosters(state.peers, state.status);
    renderMail(state.mail, state.status.now);
  } catch (err) {
    document.getElementById("me").innerHTML =
      '<span class="spinner">node unreachable&hellip;</span>';
  }
}

document.getElementById("compose").addEventListener("submit", async (event) => {
  event.preventDefault();
  const result = document.getElementById("compose-result");
  const payload = {
    to: document.getElementById("compose-to").value,
    subject: document.getElementById("compose-subject").value,
    body: document.getElementById("compose-body").value,
  };

  result.textContent = "Sending\u2026";
  const response = await fetch("/api/mail/send", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  // The server reports ambiguity in full; show it rather than a generic failure.
  result.textContent = await response.text();
  if (response.ok) {
    document.getElementById("compose-subject").value = "";
    document.getElementById("compose-body").value = "";
    refresh();
  }
});

document.addEventListener("click", async (event) => {
  const verifyButton = event.target.closest("[data-verify]");
  if (verifyButton) {
    verifyButton.disabled = true;
    await fetch(`/api/peers/${verifyButton.dataset.verify}/verify`, { method: "POST" });
    await refresh();
    return;
  }

  if (event.target.id === "run-doctor") {
    const button = event.target;
    button.disabled = true;
    button.textContent = "Listening…";
    document.getElementById("doctor").innerHTML =
      '<p class="empty">Listening on both discovery paths…</p>';
    try {
      renderDoctor(await getJson("/api/doctor"));
    } catch (err) {
      document.getElementById("doctor").innerHTML =
        '<p class="empty">The check could not run.</p>';
    }
    button.disabled = false;
    button.textContent = "Run check";
  }
});

refresh();
setInterval(refresh, REFRESH_MS);
