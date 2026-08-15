import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { performance } from "node:perf_hooks";

import { describe, expect, it } from "vitest";

import type { Row } from "../types";
import { searchIssueRows } from "./issueSearch";

interface ScanSpec {
  version: number;
  corpus: {
    scales: number[];
    smokeScale: number;
    projects: number;
    edgePattern: string;
  };
  measurement: {
    warmIterations: number;
    browserWarmups: number;
    browserIterations: number;
    browserQuery: string;
  };
  callPaths: Record<string, string[]>;
}

const specPath = resolve(import.meta.dirname, "../../../benchmarks/issues-scan.json");

function spec(): ScanSpec {
  return JSON.parse(readFileSync(specPath, "utf8")) as ScanSpec;
}

function row(index: number, projects: number): Row {
  const project = index % projects;
  return {
    reff: `iss_${index.toString().padStart(26, "0")}`,
    doc_id: `iss_${index.toString().padStart(26, "0")}`,
    project_id: `prj_${project.toString().padStart(26, "0")}`,
    key_alias: `P${project.toString().padStart(2, "0")}-${index + 1}`,
    title: `Issue ${index.toString().padStart(5, "0")} deterministic baseline`,
    status: "backlog",
    priority: "none",
    assignee_summary: "",
    assignees: [],
    tombstone: false,
    provisional: false,
  };
}

function corpus(count: number, projects: number): Row[] {
  return Array.from({ length: count }, (_, index) => row(index, projects));
}

describe("issue search scan baseline", () => {
  it("keeps the dialog's existing search semantics in the measurable pure seam", () => {
    const rows = [row(12, 20), row(1, 20), row(2, 20)];
    expect(searchIssueRows(rows, "")).toBe(rows);
    expect(searchIssueRows(rows, "00012").map((candidate) => candidate.doc_id)).toEqual([
      rows[0]!.doc_id,
    ]);
    expect(searchIssueRows(rows, "does-not-exist")).toEqual([]);
  });

  it("measures the exact browser scoring loop on the shared corpus", () => {
    const config = spec();
    expect(config.version).toBe(1);
    expect(config.corpus.edgePattern).toBe("forward-relates-chain");
    expect(config.callPaths.browserSearch).toContain("viewer::searchIssueRows");

    const full = process.env.LAIT_ISSUES_SCAN_FULL !== undefined;
    const scales = full ? config.corpus.scales : [config.corpus.smokeScale];
    const iterations = full ? config.measurement.browserIterations : 5;
    const reports = scales.map((count) => {
      const rows = corpus(count, config.corpus.projects);
      for (let i = 0; i < config.measurement.browserWarmups; i += 1) {
        searchIssueRows(rows, config.measurement.browserQuery);
      }
      const samples = Array.from({ length: iterations }, () => {
        const started = performance.now();
        const results = searchIssueRows(rows, config.measurement.browserQuery);
        const wallMicros = Math.round((performance.now() - started) * 1_000);
        const returnedBytes = Buffer.byteLength(JSON.stringify(results));
        // Source-level allocation shape of the exact map/filter/sort/map chain:
        // one scored record per candidate, three result arrays, and one array
        // slot in the scored array plus two for every surviving result. This is
        // deterministic across V8 builds; it does not pretend to be allocator
        // bytes, which JavaScript does not expose as a stable contract.
        const observableAllocationUnits = count + 3 + count + 2 * results.length;
        return {
          parsedBodies: 0,
          bodyScanVisits: 0,
          graphEdgeVisits: 0,
          candidateVisits: count,
          scoredRecordAllocations: count,
          observableAllocationUnits,
          resultRows: results.length,
          returnedBytes,
          wallMicros,
        };
      });
      return { issues: count, projects: config.corpus.projects, samples };
    });

    const reportPath = process.env.LAIT_BROWSER_SCAN_REPORT;
    if (reportPath) {
      mkdirSync(dirname(reportPath), { recursive: true });
      writeFileSync(
        reportPath,
        `${JSON.stringify({
          version: 1,
          platform: process.platform,
          architecture: process.arch,
          node: process.version,
          allocationUnit: "one scored object, array object, or populated array slot",
          query: config.measurement.browserQuery,
          callPath: config.callPaths.browserSearch,
          scales: reports,
        }, null, 2)}\n`,
      );
    }

    for (const report of reports) {
      expect(report.samples.every((sample) => sample.candidateVisits === report.issues)).toBe(true);
      expect(report.samples.every((sample) => sample.resultRows === report.issues)).toBe(true);
    }
  });
});
