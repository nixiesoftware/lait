/**
 * Syntax highlighting for issue prose.
 *
 * Three decisions, each load-bearing:
 *
 * **Tokens, not HTML.** Shiki's headline API is `codeToHtml`, which returns a
 * string you hand to `dangerouslySetInnerHTML`. `ui/Markdown.tsx` promises that
 * no string in an issue body ever reaches `innerHTML` — the safety property is
 * structural rather than an escaping discipline, and there is a test that says
 * so. `codeToTokens` returns data instead, which the renderer turns into
 * elements, so highlighting costs nothing from that promise.
 *
 * **Both themes at once.** `defaultColor: false` makes every token carry
 * `--shiki-light` and `--shiki-dark` and *no* inline `color`, so the theme is
 * chosen in CSS by the same `prefers-color-scheme` / `[data-theme]` pair the
 * rest of the app uses. Highlighting a block twice, or re-highlighting on a
 * theme switch, would otherwise be the alternative.
 *
 * **The JavaScript engine, and a fixed grammar list.** Shiki's default engine
 * is Oniguruma compiled to WASM, which the runtime fetches. The HTTP head embeds
 * its bundle in the binary and answers on loopback with no network beyond it, so
 * a fetched `.wasm` is a broken code block on an air-gapped machine. The JS
 * engine is pure bundle. The grammars are listed explicitly for the same reason
 * the full bundle is avoided: a tracker for this project needs Rust and shell,
 * not 200 languages.
 *
 * The whole module is behind a dynamic `import()`, so none of it is in the chunk
 * that draws the first screen.
 */

/** One highlighted run of characters, with a colour per theme. */
export type Token = {
  content: string;
  /** `--shiki-light` / `--shiki-dark`; consumed as an inline style object. */
  style: Record<string, string>;
};

/**
 * The grammars we carry, keyed by the name an author would write after the
 * fence. Shiki registers each grammar's own aliases too (`bash` also answers to
 * `sh`, `shell`, `zsh`), so this list is the floor, not the ceiling.
 *
 * Chosen for what actually appears in this tracker's issues: the engine is Rust,
 * the viewer is TypeScript, the reproductions are shell, and the artefacts are
 * JSON, TOML and diffs.
 */
const LANGS = {
  bash: () => import("shiki/langs/bash.mjs"),
  rust: () => import("shiki/langs/rust.mjs"),
  typescript: () => import("shiki/langs/typescript.mjs"),
  tsx: () => import("shiki/langs/tsx.mjs"),
  javascript: () => import("shiki/langs/javascript.mjs"),
  json: () => import("shiki/langs/json.mjs"),
  toml: () => import("shiki/langs/toml.mjs"),
  yaml: () => import("shiki/langs/yaml.mjs"),
  python: () => import("shiki/langs/python.mjs"),
  sql: () => import("shiki/langs/sql.mjs"),
  diff: () => import("shiki/langs/diff.mjs"),
  css: () => import("shiki/langs/css.mjs"),
  html: () => import("shiki/langs/html.mjs"),
  markdown: () => import("shiki/langs/markdown.mjs"),
} as const;

type Highlighter = Awaited<
  ReturnType<typeof import("shiki/core").createHighlighterCore>
>;

/** One highlighter for the document, built at most once. */
let pending: Promise<Highlighter> | null = null;

async function highlighter(): Promise<Highlighter> {
  // Themes and engine only. Handing `langs` to the constructor would pull every
  // grammar in the table on the first code block — a megabyte to colour three
  // lines of shell, and `typescript`, `tsx` and `javascript` are ~180 kB each.
  pending ??= (async () => {
    const [{ createHighlighterCore }, { createJavaScriptRegexEngine }] = await Promise.all([
      import("shiki/core"),
      import("shiki/engine/javascript"),
    ]);
    return createHighlighterCore({
      themes: [
        import("shiki/themes/github-dark-default.mjs"),
        import("shiki/themes/github-light-default.mjs"),
      ],
      langs: [],
      engine: createJavaScriptRegexEngine(),
    });
  })();
  return pending;
}

/** Grammars already asked for, so a page of shell blocks loads `bash` once. */
const loaded = new Map<keyof typeof LANGS, Promise<void>>();

/**
 * Bring in one grammar, resolving the alias first.
 *
 * Aliases have to be mapped *here* rather than asked of the highlighter: before
 * a grammar is loaded the highlighter has never heard of `sh`, so consulting it
 * would decline every alias exactly once and then be right forever after — the
 * kind of bug that only shows up on a cold page.
 */
const ALIASES: Record<string, keyof typeof LANGS> = {
  sh: "bash",
  shell: "bash",
  shellscript: "bash",
  zsh: "bash",
  console: "bash",
  ts: "typescript",
  js: "javascript",
  jsx: "tsx",
  rs: "rust",
  py: "python",
  yml: "yaml",
  md: "markdown",
};

function resolve(lang: string): keyof typeof LANGS | null {
  const l = lang.toLowerCase().trim();
  if (l in LANGS) return l as keyof typeof LANGS;
  return ALIASES[l] ?? null;
}

async function ensureLang(name: keyof typeof LANGS): Promise<void> {
  let load = loaded.get(name);
  if (!load) {
    load = (async () => {
      const hl = await highlighter();
      await hl.loadLanguage(await LANGS[name]());
    })();
    loaded.set(name, load);
  }
  return load;
}

/**
 * Highlight a block, or decline.
 *
 * `null` means "render this as plain text" and is the answer for an unfenced
 * language, a language we do not carry, and any failure inside Shiki. A code
 * block that renders uncoloured is a small loss; one that throws takes the
 * issue body with it.
 */
export async function highlight(code: string, lang: string | null): Promise<Token[][] | null> {
  if (!lang) return null;
  const name = resolve(lang);
  if (!name) return null;
  try {
    const hl = await highlighter();
    await ensureLang(name);
    const { tokens } = hl.codeToTokens(code, {
      lang: name,
      themes: { light: "github-light-default", dark: "github-dark-default" },
      defaultColor: false,
    });
    return tokens.map((line) =>
      line.map((token) => ({
        content: token.content,
        style: (token.htmlStyle ?? {}) as Record<string, string>,
      })),
    );
  } catch {
    return null;
  }
}

/** Whether a fence tag names something we can colour. Lets the renderer skip
 *  loading Shiki at all for a plain or unknown fence. */
export function isHighlightable(lang: string | null): boolean {
  return lang !== null && resolve(lang) !== null;
}
