#!/usr/bin/env node
// A headless signage receiver that plays a coordinator's whole-program HLS
// stream the way a strict live player does, and writes down what it saw.
//
//   node tools/display-probe/probe.mjs [--socket PATH] [--origin URL]
//        [--assignment ID] [--seconds N] [--prefetch-ms N] [--state DIR]
//        [--fresh] [--revoke] [--out FILE] [--verbose]
//
// Exit 0: no violations and no stalls. 1: some. 2: the run could not be set up.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { rendezvousFromCode } from "../../receivers/shared/web/protocol.mjs";
import { ControlSocket } from "./lib/control.mjs";
import { installNativeBridge, pemFromDer, PinnedTransport, sha256OfPem } from "./lib/transport.mjs";
import { FileVault, ProbeReceiver, ProbeUi, probeCapabilities, Stats } from "./lib/receiver.mjs";

const HERE = fileURLToPath(new URL(".", import.meta.url));
const DEFAULT_DAEMON_DIR = path.join(os.homedir(), "Library", "Application Support", "dev.nixi.lait", "daemon");

function usage(message) {
  if (message) console.error(`display-probe: ${message}\n`);
  console.error(`usage: node tools/display-probe/probe.mjs [options]
  --socket PATH       daemon control socket (default: ${path.join(DEFAULT_DAEMON_DIR, "control.sock")})
  --origin URL        coordinator origin (default: what display_status announces; must match it)
  --assignment ID     assignment whose pin the probe copies (default: the first active one)
  --seconds N         run length after start (default: 60)
  --viewport WxH      the screen size this receiver declares (default: 1280x720)
  --prefetch-ms N     fetch a segment this far before it is needed (default: one target duration)
  --stale-after-ms N  freshness pinned on the probe's assignment (default: 120000)
  --state DIR         credential + device state (default: tools/display-probe/.state)
  --fresh             discard the stored credential and pair again
  --revoke            revoke the probe's device over the socket on exit
  --out FILE          write the JSON report here (default: report.json in --state)
  --verbose           log every protocol event`);
  process.exit(2);
}

function parseArgs(argv) {
  const options = {
    socket: path.join(DEFAULT_DAEMON_DIR, "control.sock"),
    origin: null,
    assignment: null,
    seconds: 60,
    prefetchMs: null,
    staleAfterMs: 120_000,
    state: path.join(HERE, ".state"),
    fresh: false,
    revoke: false,
    out: null,
    verbose: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const next = () => {
      index += 1;
      if (index >= argv.length) usage(`${argument} needs a value`);
      return argv[index];
    };
    switch (argument) {
      case "--socket": options.socket = next(); break;
      case "--origin": options.origin = next(); break;
      case "--assignment": options.assignment = next(); break;
      case "--seconds": options.seconds = Number(next()); break;
      case "--viewport": {
        const match = /^(\d+)x(\d+)$/.exec(next());
        if (!match) usage("--viewport must look like 1920x1080");
        options.viewport = { width: Number(match[1]), height: Number(match[2]) };
        break;
      }
      case "--prefetch-ms": options.prefetchMs = Number(next()); break;
      case "--stale-after-ms": options.staleAfterMs = Number(next()); break;
      case "--state": options.state = path.resolve(next()); break;
      case "--fresh": options.fresh = true; break;
      case "--revoke": options.revoke = true; break;
      case "--out": options.out = path.resolve(next()); break;
      case "--verbose": options.verbose = true; break;
      case "--help": case "-h": usage(); break;
      default: usage(`unknown argument ${argument}`);
    }
  }
  if (!Number.isFinite(options.seconds) || options.seconds <= 0) usage("--seconds must be a positive number");
  if (options.prefetchMs !== null && (!Number.isFinite(options.prefetchMs) || options.prefetchMs < 0)) usage("--prefetch-ms must be >= 0");
  options.out ??= path.join(options.state, "report.json");
  return options;
}

function readJson(file) {
  return fs.existsSync(file) ? JSON.parse(fs.readFileSync(file, "utf8")) : null;
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function formatMs(value) {
  return value == null ? "-" : `${value} ms`;
}

function summary(report) {
  const lines = [];
  const push = (line) => lines.push(line);
  push(`display-probe ${report.probe.build} against ${report.probe.origin} — ${report.coordinator.label} (${report.coordinator.instance.slice(0, 8)}), daemon ${report.coordinator.daemon?.version ?? "?"}`);
  push(`device ${report.probe.device ?? "?"}${report.probe.reused_credential ? " (credential reused)" : " (paired this run)"}; assignment copied from ${report.probe.assignment_copied}`);
  push(`ran ${Math.round(report.probe.ran_ms / 1000)} s, exit ${report.exit_code}${report.fatal ? ` — FATAL ${report.fatal.code}: ${report.fatal.message}` : ""}`);
  push("");
  push(`startup     pairing ${formatMs(report.startup.pairing_ms)} · pair→first segment ${formatMs(report.startup.pair_to_first_segment_ms)} · start→first segment ${formatMs(report.startup.start_to_first_segment_ms)}`);
  const marks = Object.entries(report.startup.marks_ms);
  if (marks.length) push(`            ${marks.map(([name, at]) => `${name} +${at}`).join(" · ")}`);
  const first = report.startup.first_playlist;
  if (first) {
    push(`playlist    ${report.playlist.rendition} target ${report.playlist.target_duration_ms} ms · first window ${first.media_sequence}..${first.end_sequence} (${first.segments} segs), started at ${first.start_sequence} · ${report.playlist.reloads} reloads, ${report.playlist.refused} refused · window ${report.playlist.window_segments.min}..${report.playlist.window_segments.max} segs`);
  }
  push(`runway      min ${formatMs(report.runway_ms.min)} · median ${formatMs(report.runway_ms.median)} · max ${formatMs(report.runway_ms.max)}`);
  push(`stalls      ${report.stalls.count} (${report.stalls.total_ms} ms total)${report.stalls.list.length ? ` — ${report.stalls.list.slice(0, 5).map((stall) => `${stall.reason}@${stall.sequence} ${stall.duration_ms}ms`).join(", ")}` : ""}`);
  push(`segments    ${report.segments.fetched} fetched, ${report.segments.failed} failed, ${(report.segments.bytes / 1_048_576).toFixed(1)} MiB · latency p50 ${formatMs(report.segments.latency_ms.p50)} p95 ${formatMs(report.segments.latency_ms.p95)} max ${formatMs(report.segments.latency_ms.max)}`);
  push(`violations  ${report.violations.total}`);
  for (const [kind, entry] of Object.entries(report.violations.by_kind)) {
    push(`            ${kind} ×${entry.count} — first at ${entry.first.at_ms} ms: ${entry.first.detail}`);
  }
  push(`recoveries  ${report.recoveries.length}${report.recoveries.map((recovery) => ` — ${recovery.cause} in ${recovery.recovery_ms} ms, ${recovery.refused_requests} refused`).join("")}${report.recovery_in_progress ? ` — one still open (${report.recovery_in_progress.cause})` : ""}`);
  push(`health      ${report.health.accepted} accepted, ${report.health.refused} refused`);
  push(`poll        ${Object.entries(report.poll).map(([kind, count]) => `${kind} ${count}`).join(" · ")}`);
  const refusals = Object.entries(report.api_refusals);
  if (refusals.length) push(`api refused ${refusals.map(([code, count]) => `${code} ${count}`).join(" · ")}`);
  if (report.coordinator.playlist_comments.length) push(`provenance  ${report.coordinator.playlist_comments.join(" | ")}`);
  return lines.join("\n");
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const log = (line) => {
    if (options.verbose) console.error(`[${new Date().toISOString().slice(11, 23)}] ${line}`);
  };
  const say = (line) => console.error(line);

  // ---- setup: the coordinator's facts, over the socket ----
  let control;
  try {
    control = await ControlSocket.connect(options.socket);
  } catch (error) {
    say(`display-probe: cannot open ${options.socket}: ${error.message} (is \`lait daemon\` running?)`);
    process.exit(2);
  }
  const hello = await control.hello();
  const status = await control.daemon("display_status");
  const origin = options.origin ?? status.origin;
  if (origin !== status.origin) {
    say(`display-probe: note: --origin ${origin} differs from the announced ${status.origin}; the coordinator's /head/v1/instance names the latter and a pinned receiver refuses the mismatch`);
  }
  const pem = status.certificate_pem;
  const sha256 = status.certificate_sha256 || sha256OfPem(pem);
  if (sha256OfPem(pem) !== sha256) {
    say("display-probe: display_status certificate_pem does not digest to certificate_sha256");
    process.exit(2);
  }
  // The on-disk TLS record is the fallback provenance when a daemon predates
  // `certificate_pem` on the status view.
  const tlsFile = path.join(path.dirname(options.socket), "display", "tls", "coordinator-tls.json");
  const tls = readJson(tlsFile);
  if (tls && !status.certificate_pem) status.certificate_pem = pemFromDer(tls.certificate_der);

  const now = Date.now();
  // An earlier probe's own pin is a copy, never the thing to copy.
  const probeDevices = new Set(status.devices.filter((device) => device.label.startsWith("probe ")).map((device) => device.device));
  const active = status.assignments.filter((assignment) => assignment.revoked_at_unix_ms == null
    && (assignment.expires_at_unix_ms == null || assignment.expires_at_unix_ms > now)
    && !probeDevices.has(assignment.device));
  const source = options.assignment
    ? status.assignments.find((assignment) => assignment.assignment === options.assignment)
    : active[0];
  if (!source) {
    say(options.assignment
      ? `display-probe: assignment ${options.assignment} is not on this coordinator`
      : "display-probe: the coordinator has no active assignment to copy; assign a screen in Astrolabe first");
    process.exit(2);
  }
  options.assignment = source.assignment;

  const coordinator = {
    instance: status.instance,
    label: status.label,
    profile: status.coordinator_profile ?? null,
    origin: status.origin,
    certificate_sha256: sha256,
    daemon: hello.build ?? null,
    control_protocol_version: hello.protocol_version,
    copied_from: { assignment: source.assignment, device: source.device, world: source.world, surface: source.surface, orbit: source.orbit },
  };

  // ---- state ----
  fs.mkdirSync(options.state, { recursive: true });
  const credentialFile = path.join(options.state, "credential.json");
  const deviceFile = path.join(options.state, "device.json");
  if (options.fresh) {
    fs.rmSync(credentialFile, { force: true });
    fs.rmSync(deviceFile, { force: true });
  }
  let stored = readJson(credentialFile);
  if (stored && stored.origin !== origin) {
    say(`display-probe: stored credential is for ${stored.origin}, not ${origin}; pairing again`);
    fs.rmSync(credentialFile, { force: true });
    fs.rmSync(deviceFile, { force: true });
    stored = null;
  }
  const reusedCredential = Boolean(stored && stored.mode === "paired");

  // ---- enrol: a rendezvous pinned to a copy of the source assignment ----
  let rendezvous = null;
  if (!reusedCredential) {
    const world = await control.daemon("display_world_receivers", { world: source.world, orbit: source.orbit });
    const receiver = world.receivers.find((candidate) => candidate.assignment && candidate.assignment.assignment === source.assignment);
    if (!receiver) {
      say(`display-probe: ${source.world} does not report the input of assignment ${source.assignment}`);
      process.exit(2);
    }
    const assignment = {
      orbit: source.orbit,
      world: source.world,
      surface: source.surface,
      input: receiver.assignment.input,
      theme: source.theme,
      stale_after_ms: options.staleAfterMs,
      on_stale: "keep_with_native_banner",
      sync: source.sync ? { group: source.sync.group, mode: source.sync.mode, static_delay_ms: source.sync.static_delay_ms } : null,
      expires_at_unix_ms: null,
    };
    const minted = await control.daemon("display_rendezvous_mint", { label: `probe ${os.hostname()}`, assignment });
    rendezvous = minted.rendezvous;
    const derived = await rendezvousFromCode(minted.code);
    if (derived !== rendezvous) {
      say(`display-probe: minted rendezvous ${rendezvous} is not what code ${minted.code} derives to (${derived}); using the derived id`);
      rendezvous = derived;
    }
    log(`rendezvous ${minted.code} minted, pinned to ${source.world}/${source.surface} ${JSON.stringify(receiver.assignment.input)}`);
  }

  const bootstrap = {
    protocol_major: 1,
    trust: { kind: "pinned_certificate", origin, sha256 },
    certificate_pem: pem,
    rendezvous,
  };

  // ---- run ----
  const transport = new PinnedTransport({ pem, sha256 });
  installNativeBridge(transport);
  const stats = new Stats({ log });
  transport.onResponse = (response) => {
    if (options.verbose && response.status >= 400) log(`${response.method} ${response.path} → ${response.status}`);
  };

  let finish;
  const finished = new Promise((resolve) => { finish = resolve; });
  const ui = new ProbeUi({
    log,
    stats,
    onFatal: (fatal) => {
      if (!stats.fatal) stats.fatal = fatal;
      finish("fatal");
    },
  });
  const receiver = new ProbeReceiver({
    bootstrap,
    capabilities: probeCapabilities(options.viewport ?? {}),
    ui,
    vault: new FileVault(credentialFile),
    transport,
    stats,
    log,
    // `null` means one target duration, which a session learns from its
    // first playlist.
    prefetchMs: options.prefetchMs,
  });

  say(`display-probe: ${reusedCredential ? "resuming" : "pairing"} against ${origin} for ${options.seconds} s (assignment ${source.assignment}, ${source.world})`);
  const deadline = setTimeout(() => finish("deadline"), options.seconds * 1000);
  process.once("SIGINT", () => finish("interrupt"));
  receiver.start().catch((error) => ui.showFailure(error.code || "internal", error.message));
  const why = await finished;
  clearTimeout(deadline);
  receiver.stop();
  log(`stopping: ${why}`);

  // ---- tidy ----
  const device = receiver.credential?.device ?? stored?.device ?? null;
  if (device) writeJson(deviceFile, { device, origin, assignment_copied: source.assignment, paired_at_unix_ms: stats.startedAtUnixMs });
  if (options.revoke && device) {
    try {
      await control.daemon("display_device_revoke", { device });
      fs.rmSync(credentialFile, { force: true });
      fs.rmSync(deviceFile, { force: true });
      say(`display-probe: revoked device ${device}`);
    } catch (error) {
      say(`display-probe: revoke failed: ${error.message}`);
    }
  }
  control.close();

  const report = stats.report({ options: { ...options, origin }, coordinator, device, reusedCredential });
  writeJson(options.out, report);
  console.log(summary(report));
  console.log(`\nreport: ${options.out}`);
  process.exit(report.exit_code);
}

main().catch((error) => {
  console.error(`display-probe: ${error.stack || error}`);
  process.exit(2);
});
