import { useEffect, useState } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ProgramEditor } from "@/program-editor";
import { fetchProgram, saveProgram } from "@/utils/broadcasts/api";
import { fetchLibrary } from "@/utils/content/api";
import { draftNameKey } from "@/utils/navigation/actions";
import { goBack } from "@/utils/navigation/goBack";
import type { SignageMedia, SignageProgram } from "@/utils/lait/types";

export default function BroadcastPage() {
  const params = useParams({ strict: false });
  const programId = String(params.id ?? "");
  const navigate = useNavigate();
  const [program, setProgram] = useState<SignageProgram | null>(null);
  const [library, setLibrary] = useState<SignageMedia[]>([]);
  const [persisted, setPersisted] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");

  const refreshLibrary = async (): Promise<SignageMedia[]> => {
    const catalog = await fetchLibrary();
    setLibrary(catalog);
    return catalog;
  };

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      setLoading(true);
      setError("");
      try {
        const [loaded, catalog] = await Promise.all([
          fetchProgram(programId),
          fetchLibrary(),
        ]);
        if (cancelled) return;
        setLibrary(catalog);
        if (loaded) {
          setProgram(loaded.program);
          setPersisted(true);
          return;
        }
        const draftName = sessionStorage.getItem(draftNameKey(programId));
        if (!draftName) {
          setError("This program is not on the World, and no draft name was kept.");
          return;
        }
        setPersisted(false);
        setProgram({
          id: programId,
          name: draftName,
          cycle: "loop",
          items: [],
          windows: [],
        });
      } catch (err) {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : "The program could not be loaded.");
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, [programId]);

  if (loading) {
    return <p className="pe-hint">Loading program…</p>;
  }
  if (error || !program) {
    return <p className="pe-error">{error || "Missing program."}</p>;
  }

  return (
    <ProgramEditor
      initial={program}
      library={library}
      persisted={persisted}
      onRefreshLibrary={refreshLibrary}
      onClose={() => goBack(navigate, "/broadcast-list")}
      onSave={async (next) => {
        if (next.items.length === 0) {
          throw new Error("A program needs at least one item before it can be saved.");
        }
        await saveProgram(next);
        sessionStorage.removeItem(draftNameKey(programId));
        setProgram(next);
        setPersisted(true);
      }}
    />
  );
}
