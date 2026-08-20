import { useState, type ReactNode } from "react";

import { type ClientAction, type ClientView, type McpBinding } from "./client";
import { Notice, shortId } from "./kit";

export type SecondarySurface = "book" | "displays" | "record";

type SurfaceProps = {
  view: ClientView;
  dispatch(action: ClientAction): Promise<void>;
  onBack(): void;
  ownedWindow?: boolean;
};

export function RecordSurface({ view, dispatch, onBack }: SurfaceProps) {
  return <SecondaryFrame title="Local record" detail="Supervised devices, browser heads, Spaces, and authored MCP bindings." onBack={onBack} actions={<button className="quiet-button" onClick={() => void dispatch({ type: "refresh" })}>Refresh</button>}>
    <div className="secondary-scroll record-surface"><section className="section-block"><div className="section-heading"><span>DEVICES</span><span>{view.devices.length}</span></div><div className="record-grid">{view.devices.map((device) => <DeviceCard key={device.id} device={device} dispatch={dispatch} />)}</div>{view.devices.some((device) => device.owned) && <button className="danger-button" onClick={() => { if (window.confirm("Stop every device this client owns?")) void dispatch({ type: "stopAllOwned" }); }}>Stop all owned devices</button>}</section>
      <section className="section-block"><div className="section-heading"><span>HEADS</span><span>{view.heads.length}</span></div><div className="button-row"><button className="primary-button" onClick={() => void dispatch({ type: "startHead" })}>Start browser head</button>{view.heads.map((head) => <span className="token" key={head.id}>{head.kind} · {shortId(head.id)}{head.owned && <button onClick={() => void dispatch({ type: "stopHead", id: head.id })}>×</button>}</span>)}</div></section>
      <section className="section-block"><div className="section-heading"><span>SPACES</span><span>{view.orbits.length}</span></div>{view.orbits.map((orbit) => <div className="orbit-row" key={orbit.space}><span><strong>{orbit.name}</strong><small>{shortId(orbit.space)} · {orbit.path}</small></span><div className="button-row"><button className="quiet-button" onClick={() => void dispatch({ type: "readSpace", orbit: orbit.space })}>Inspect</button><button className="danger-button" onClick={() => { if (window.confirm(`Forget ${orbit.name}? Its store remains on disk.`)) void dispatch({ type: "forgetOrbit", space: orbit.space }); }}>Forget</button></div></div>)}{view.space !== null && <SpaceFacts view={view} />}</section>
      <McpPanel binding={view.mcp} worlds={view.library ?? []} dispatch={dispatch} />
    </div>
  </SecondaryFrame>;
}

function DeviceCard({ device, dispatch }: { device: ClientView["devices"][number]; dispatch(action: ClientAction): Promise<void> }) {
  return <article className="device-card"><strong>{device.label}</strong><small>{device.state} · {shortId(device.id)}</small>{device.degraded !== null && <Notice tone="warn">{device.degraded}</Notice>}{device.lastError !== null && <Notice tone="danger">{device.lastError}</Notice>}<div className="button-row">{device.owned ? <><button className="quiet-button" onClick={() => void dispatch({ type: "stopDevice", id: device.id })}>Stop</button><button className="quiet-button" onClick={() => void dispatch({ type: "restartDevice", id: device.id })}>Restart</button>{device.canForceStop && <button className="danger-button" onClick={() => void dispatch({ type: "forceStopDevice", id: device.id })}>Force stop</button>}</> : <span className="muted">Not owned by this client</span>}<button className="text-button danger-text" onClick={() => { if (window.confirm(`Remove ${device.label}?`)) void dispatch({ type: "removeDevice", id: device.id, deleteData: false }); }}>Remove</button></div></article>;
}

function SpaceFacts({ view }: { view: ClientView }) {
  const space = view.space!;
  return <div className="space-facts"><strong>{space.whoami ?? "No local standing"}</strong><p>{space.diagnosis?.summary ?? "No diagnosis has been read."}</p>{space.diagnosis?.gates.map((gate) => <span className={`gate gate-${gate.state}`} key={gate.id}>{gate.label}: {gate.detail}</span>)}<div className="member-list">{space.members.map((member) => <span key={member.id}>{member.authoredName ?? member.nick ?? shortId(member.id)}{member.admin ? " · admin" : ""}</span>)}</div></div>;
}

function McpPanel({ binding, worlds, dispatch }: { binding: McpBinding | null; worlds: NonNullable<ClientView["library"]>; dispatch(action: ClientAction): Promise<void> }) {
  const [client, setClient] = useState("claude"); const [name, setName] = useState("lait"); const [project, setProject] = useState(""); const [world, setWorld] = useState("");
  return <section className="section-block"><div className="section-heading">MCP BINDING</div>{binding !== null && <Notice tone="good">{binding.written ? "Written" : "Preview"}: {binding.detail}</Notice>}<div className="mcp-form"><select value={client} onChange={(event) => setClient(event.target.value)}><option value="claude">Claude</option><option value="cursor">Cursor</option><option value="windsurf">Windsurf</option><option value="generic">Generic</option></select><input value={name} onChange={(event) => setName(event.target.value)} placeholder="Binding name" /><input value={project} onChange={(event) => setProject(event.target.value)} placeholder="Project path" /><select value={world} onChange={(event) => setWorld(event.target.value)}><option value="">Default World</option>{worlds.map((item) => <option key={item.worldMount} value={item.worldMount}>{item.displayName}</option>)}</select><div className="button-row"><button className="quiet-button" disabled={name.trim() === "" || project.trim() === ""} onClick={() => void dispatch({ type: "installMcp", client, scope: null, name: name.trim(), agent: null, noAgent: false, project: project.trim(), world: world || null, preview: true })}>Preview</button><button className="primary-button" disabled={name.trim() === "" || project.trim() === ""} onClick={() => void dispatch({ type: "installMcp", client, scope: null, name: name.trim(), agent: null, noAgent: false, project: project.trim(), world: world || null, preview: false })}>Write binding</button></div></div></section>;
}

function SecondaryFrame({ title, detail, onBack, backLabel = "← Library", actions, children }: { title: string; detail: string; onBack(): void; backLabel?: string; actions?: ReactNode; children: ReactNode }) { return <section className="secondary-surface"><header className="secondary-header"><button className="back-button" onClick={onBack}>{backLabel}</button><div><h1>{title}</h1><p>{detail}</p></div><div className="header-actions">{actions}</div></header>{children}</section>; }
