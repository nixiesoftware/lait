import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { type ResourceKey, type ResourceSnapshot, WorldViewStore } from "./worldViewStore";

const WorldViewContext = createContext<WorldViewStore | null>(null);

export function WorldViewStoreProvider({
  store,
  children,
}: {
  store: WorldViewStore;
  children: ReactNode;
}) {
  return <WorldViewContext.Provider value={store}>{children}</WorldViewContext.Provider>;
}

export function useWorldViewStore(): WorldViewStore {
  const store = useContext(WorldViewContext);
  if (!store) throw new Error("WorldViewStoreProvider is missing");
  return store;
}

export function useWorldResource<T>(
  key: ResourceKey,
  loader?: () => Promise<T>,
): ResourceSnapshot<T> {
  const store = useWorldViewStore();
  const snapshot = useSyncExternalStore(
    (listener) => store.subscribe(key, listener),
    () => store.read<T>(key),
    () => store.read<T>(key),
  );
  useEffect(() => {
    if (loader) void store.ensure(key, loader).catch(() => undefined);
  }, [key, loader, store]);
  // A load that failed is asked once more when this resource is next observed.
  //
  // `ensure` leaves a rejected entry stale with no promise in flight, so it is
  // ready to be retried — and nothing retried it. The effect above depends on
  // the key, the loader and the store, none of which change because a request
  // failed, so a resource that lost one race stayed lost for as long as the
  // view did. A transient failure at startup was indistinguishable from a
  // permanent one, and the only ways out were navigating elsewhere or finding a
  // Retry button.
  //
  // Once per failure. What keeps that true is mostly the dependency list: a
  // rejection publishes a new snapshot, but `snapshot.state` is still the
  // string "error", so this effect does not re-run and cannot spin against a
  // daemon that is already struggling. Depending on `snapshot.error` instead
  // would spin, because every rejection carries a fresh error object.
  //
  // The ref bounds the rest: any *other* reason this effect re-runs while the
  // resource is still failing — a re-mount, a new loader identity — would
  // otherwise spend another attempt. Cleared on a success, so a later failure
  // is a new episode with its own attempt.
  const retriedFor = useRef<ResourceKey | null>(null);
  useEffect(() => {
    if (snapshot.state === "ready") {
      retriedFor.current = null;
      return;
    }
    if (!loader || snapshot.state !== "error" || retriedFor.current === key) return;
    retriedFor.current = key;
    void store.ensure(key, loader).catch(() => undefined);
  }, [key, loader, store, snapshot.state]);
  return snapshot;
}

/**
 * Observe a dynamic set of resources as one React dependency.
 *
 * Calling `useWorldResource` in a loop would make the number of hooks depend on
 * the number of projects. Loading the resources without observing them is just
 * as wrong: `ProjectViewerStore` refreshes only active keys, so an unobserved
 * fan-out is a one-shot snapshot that never hears another doorbell.
 *
 * This is one external-store subscription whose membership may change. Every
 * member key remains independently cached and independently invalidated; the
 * returned array changes identity only when one of those member snapshots does,
 * which is the stability contract `useSyncExternalStore` requires.
 */
export function useWorldResources<T>(
  keys: readonly ResourceKey[],
  loader?: (key: ResourceKey, index: number) => Promise<T>,
): readonly ResourceSnapshot<T>[] {
  const store = useWorldViewStore();
  const signature = JSON.stringify(keys);
  const stableKeys = useMemo(() => [...keys], [signature]);
  const observer = useMemo(() => {
    let held: readonly ResourceSnapshot<T>[] | null = null;
    const read = (): readonly ResourceSnapshot<T>[] => {
      const next = stableKeys.map((key) => store.read<T>(key));
      if (held && held.length === next.length && held.every((value, i) => value === next[i])) {
        return held;
      }
      held = Object.freeze(next);
      return held;
    };
    const subscribe = (listener: () => void) => {
      const unsubscribes = stableKeys.map((key) => store.subscribe(key, listener));
      return () => {
        for (const unsubscribe of unsubscribes) unsubscribe();
      };
    };
    return { read, subscribe };
  }, [stableKeys, store]);
  const snapshots = useSyncExternalStore(observer.subscribe, observer.read, observer.read);
  useEffect(() => {
    if (!loader) return;
    stableKeys.forEach((key, index) => {
      void store.ensure(key, () => loader(key, index)).catch(() => undefined);
    });
  }, [loader, stableKeys, store]);
  return snapshots;
}
