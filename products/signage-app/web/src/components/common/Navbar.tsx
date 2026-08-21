import React, { useState, useEffect, useRef, useCallback } from "react";
import { useNavigate } from "@tanstack/react-router";
import { SearchIcon, Monitor, Radio, Image, FileText, X } from "lucide-react";
import { fetchPrograms } from "@/utils/broadcasts/api";
import { fetchScreens } from "@/utils/screens/api";
import { fetchLibrary } from "@/utils/content/api";

interface SearchResult {
  id: number | string;
  name: string;
  type: "screen" | "broadcast" | "content" | "page";
  route: string;
  meta?: string;
}

const STATIC_PAGES: SearchResult[] = [
  { id: "page-screens", name: "Screens", type: "page", route: "/screen-list" },
  { id: "page-broadcasts", name: "Broadcasts", type: "page", route: "/broadcast-list" },
  { id: "page-media", name: "Media", type: "page", route: "/content-list" },
  { id: "page-integrations", name: "Integrations", type: "page", route: "/integrations" },
];

const TYPE_ICONS: Record<string, React.ReactNode> = {
  screen: <Monitor className="size-4 text-gray-400 shrink-0" />,
  broadcast: <Radio className="size-4 text-gray-400 shrink-0" />,
  content: <Image className="size-4 text-gray-400 shrink-0" />,
  page: <FileText className="size-4 text-gray-400 shrink-0" />,
};

const TYPE_LABELS: Record<string, string> = {
  page: "Pages",
  screen: "Screens",
  broadcast: "Broadcasts",
  content: "Media",
};

export default function Navbar() {
  const navigate = useNavigate();
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResult[]>([]);
  const [isOpen, setIsOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const containerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // Close dropdown on outside click
  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setIsOpen(false);
      }
    };
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, []);

  const searchData = useCallback(
    async (q: string) => {
      const lower = q.toLowerCase().trim();
      if (!lower) {
        setResults([]);
        return;
      }

      // Match static pages
      const pageResults = STATIC_PAGES.filter((p) =>
        p.name.toLowerCase().includes(lower)
      );

      // Fetch live data in parallel
      let screenResults: SearchResult[] = [];
      let broadcastResults: SearchResult[] = [];
      let contentResults: SearchResult[] = [];

      try {
        const [screens, broadcasts, content] = await Promise.all([
          fetchScreens().catch(() => []),
          fetchPrograms().catch(() => []),
          fetchLibrary().catch(() => []),
        ]);

        screenResults = (screens || [])
          .filter((s) => (s.name || "").toLowerCase().includes(lower))
          .slice(0, 5)
          .map((s) => ({
            id: s.id,
            name: s.name || "Unnamed Screen",
            type: "screen" as const,
            route: `/screen-list`,
          }));

        broadcastResults = (broadcasts || [])
          .filter((b) => (b.name || "").toLowerCase().includes(lower))
          .slice(0, 5)
          .map((b) => ({
            id: b.id,
            name: b.name || "Untitled Broadcast",
            type: "broadcast" as const,
            route: `/broadcast-list`,
          }));

        contentResults = (content || [])
          .filter((c) => c.name.toLowerCase().includes(lower))
          .slice(0, 5)
          .map((c) => ({
            id: c.id,
            name: c.name || "Untitled",
            type: "content" as const,
            route: `/content-list`,
            meta: c.source.source,
          }));
      } catch {
        // Silently fail — still show page results
      }

      setResults([...pageResults, ...screenResults, ...broadcastResults, ...contentResults]);
    },
    []
  );

  // Debounced search
  useEffect(() => {
    if (!query.trim()) {
      setResults([]);
      return;
    }
    const timer = setTimeout(() => searchData(query), 200);
    return () => clearTimeout(timer);
  }, [query, searchData]);

  const handleSelect = (result: SearchResult) => {
    setIsOpen(false);
    setQuery("");
    setResults([]);

    if (result.type === "page") {
      navigate({ to: result.route });
    } else {
      // Navigate to the list page with the search query pre-filled
      navigate({ to: result.route, search: { q: result.name } });
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (!isOpen || results.length === 0) {
      if (e.key === "Escape") {
        setIsOpen(false);
        inputRef.current?.blur();
      }
      return;
    }

    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIndex((prev) => (prev + 1) % results.length);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIndex((prev) => (prev <= 0 ? results.length - 1 : prev - 1));
    } else if (e.key === "Enter" && activeIndex >= 0) {
      e.preventDefault();
      handleSelect(results[activeIndex]);
    } else if (e.key === "Escape") {
      setIsOpen(false);
      inputRef.current?.blur();
    }
  };

  // Group results by type for display
  const grouped = results.reduce<Record<string, SearchResult[]>>((acc, r) => {
    if (!acc[r.type]) acc[r.type] = [];
    acc[r.type].push(r);
    return acc;
  }, {});

  const typeOrder = ["page", "screen", "broadcast", "content"];
  let flatIndex = 0;


  return (
    <nav className="grid grid-cols-[1fr_auto_1fr] items-center px-6 py-3">
      <div />
      <div ref={containerRef} className="relative w-full max-w-md min-w-[280px] hidden sm:block">
        <SearchIcon className="absolute left-3 top-1/2 -translate-y-1/2 size-4 text-gray-400 pointer-events-none z-10" />
        <input
          ref={inputRef}
          type="text"
          placeholder="Search..."
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setIsOpen(true);
            setActiveIndex(-1);
          }}
          onFocus={() => { if (query.trim()) setIsOpen(true); }}
          onKeyDown={handleKeyDown}
          className="w-full rounded-lg border border-gray-200 bg-white py-2 pl-9 pr-8 text-sm text-gray-700 placeholder:text-gray-400 focus:border-brand-300 focus:outline-none focus:ring-1 focus:ring-brand-300 dark:border-gray-700 dark:bg-gray-800 dark:text-gray-300 dark:placeholder:text-gray-500"
        />
        {query && (
          <button
            onClick={() => { setQuery(""); setResults([]); setIsOpen(false); }}
            className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600"
          >
            <X className="size-3.5" />
          </button>
        )}

        {/* Search results dropdown */}
        {isOpen && results.length > 0 && (
          <div className="absolute top-full left-0 right-0 mt-1 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-lg overflow-hidden z-50 max-h-[360px] overflow-y-auto">
            {typeOrder.map((type) => {
              const group = grouped[type];
              if (!group || group.length === 0) return null;
              return (
                <div key={type}>
                  <div className="px-3 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-gray-400 dark:text-gray-500 bg-gray-50 dark:bg-gray-900">
                    {TYPE_LABELS[type] || type}
                  </div>
                  {group.map((result) => {
                    const idx = flatIndex++;
                    return (
                      <button
                        key={`${result.type}-${result.id}`}
                        onClick={() => handleSelect(result)}
                        onMouseEnter={() => setActiveIndex(idx)}
                        className={`w-full flex items-center gap-2.5 px-3 py-2 text-left text-sm transition-colors ${
                          idx === activeIndex
                            ? "bg-brand-50 dark:bg-brand-900/20"
                            : "hover:bg-gray-50 dark:hover:bg-gray-700/50"
                        }`}
                      >
                        {TYPE_ICONS[result.type]}
                        <span className="truncate text-gray-800 dark:text-gray-200">{result.name}</span>
                        {result.meta && (
                          <span className="ml-auto text-[10px] text-gray-400 dark:text-gray-500 shrink-0">
                            {result.meta}
                          </span>
                        )}
                      </button>
                    );
                  })}
                </div>
              );
            })}
          </div>
        )}

        {/* No results state */}
        {isOpen && query.trim() && results.length === 0 && (
          <div className="absolute top-full left-0 right-0 mt-1 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-lg z-50 p-4 text-center text-sm text-gray-400">
            No results found
          </div>
        )}
      </div>
    </nav>
  );
}
