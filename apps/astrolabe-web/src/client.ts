/** A small mirror of the primary Flutter client's projection. */
export interface LibraryWorld {
  key: string;
  worldMount: string;
  displayName: string;
  opensAt: string | null;
  accent: string | null;
  version: number | null;
  people: null | [];
}

export interface Head {
  id: string;
  origin: string;
  owned: boolean;
  orbit: string | null;
}

export interface ClientView {
  loading: boolean;
  library: LibraryWorld[] | null;
  heads: Head[];
  host: { version: string; orbitCount: number } | null;
  inFlight: string[];
  failures: string[];
  stale: boolean;
  notices: string[];
}

/** The canonical bundled Issues package, pending the generated browser contract. */
export const cannedClientView: ClientView = {
  loading: false,
  library: [{
    key: "issues",
    worldMount: "issues",
    displayName: "Issues",
    opensAt: "/",
    accent: "#5b8def",
    version: null,
    people: [],
  }],
  heads: [],
  host: null,
  inFlight: [],
  failures: [],
  stale: false,
  notices: [],
};

export const actionKey = {
  refresh: "refresh",
  open: (entryPath: string) => `open:${entryPath}`,
  stopHead: (id: string) => `head.stop:${id}`,
} as const;
