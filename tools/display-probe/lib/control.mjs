// The daemon's unauthenticated control socket: newline-delimited JSON, one
// connection serving many request/response pairs. Opening the socket is the
// credential (filesystem permissions are the gate), which is why the probe can
// mint a rendezvous without a head token. See `src/control.rs`.

import net from "node:net";

export const CONTROL_PROTOCOL_VERSION = 15;

export class ControlSocket {
  constructor(socket) {
    this.socket = socket;
    this.buffer = "";
    this.waiters = [];
    this.closed = null;
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      this.buffer += chunk;
      let newline;
      while ((newline = this.buffer.indexOf("\n")) >= 0) {
        const line = this.buffer.slice(0, newline);
        this.buffer = this.buffer.slice(newline + 1);
        const waiter = this.waiters.shift();
        if (waiter) waiter.resolve(line);
      }
    });
    const fail = (error) => {
      this.closed = error || new Error("control socket closed");
      for (const waiter of this.waiters.splice(0)) waiter.reject(this.closed);
    };
    socket.on("error", fail);
    socket.on("close", () => fail(null));
  }

  static connect(path) {
    return new Promise((resolve, reject) => {
      const socket = net.createConnection(path);
      socket.once("connect", () => resolve(new ControlSocket(socket)));
      socket.once("error", reject);
    });
  }

  async hello() {
    const reply = await this.request({ cmd: "hello", protocol_version: CONTROL_PROTOCOL_VERSION });
    if (reply.kind !== "hello") throw new Error(`daemon did not answer the handshake: ${JSON.stringify(reply)}`);
    return reply;
  }

  /** A daemon-scope request; the route is mandatory on this protocol. */
  daemon(cmd, fields = {}) {
    return this.request({ route: { scope: "daemon" }, cmd, ...fields });
  }

  async request(object) {
    if (this.closed) throw this.closed;
    const line = await new Promise((resolve, reject) => {
      this.waiters.push({ resolve, reject });
      this.socket.write(`${JSON.stringify(object)}\n`);
    });
    let reply;
    try {
      reply = JSON.parse(line);
    } catch {
      throw new Error(`control socket answered non-JSON: ${line.slice(0, 200)}`);
    }
    if (reply.kind === "error") {
      const error = new Error(`daemon refused ${object.cmd}: ${reply.message}`);
      error.errorKind = reply.error_kind;
      throw error;
    }
    return reply;
  }

  close() {
    this.socket.end();
    this.socket.destroy();
  }
}
