/**
 * A throwaway harness for the Astryx spike — NOT part of the app.
 *
 * The only question it answers: with `laitTheme` applied and no per-component
 * styling of any kind, does an Astryx surface still read as lait? Everything
 * here is stock Astryx — the `urgent`/`high`/`medium`/`low` variants included,
 * which exist because the theme declared them, not because this file styled
 * anything.
 *
 * Served by `proof.html`. Delete both with the branch.
 */

// The app's real stylesheet — subsetted @font-face, the declared cascade
// order, Astryx, our theme and the density layer. Importing the pieces
// individually here would test a cascade the app does not have.
import "../styles.css";

import {
  Badge,
  Button,
  Card,
  Icon,
  IconButton,
  Link,
  Popover,
  SegmentedControl,
  SegmentedControlItem,
  Heading,
  Kbd,
  StatusDot,
  Table,
  TableBody,
  TableCell,
  TableHeader,
  TableHeaderCell,
  TableRow,
  Text,
  Theme,
} from "@astryxdesign/core";
import { useState } from "react";
import { createRoot } from "react-dom/client";

import { DatePicker } from "../ui/DatePicker";
import { laitTheme } from "./lait";

type Priority = "urgent" | "high" | "medium" | "low";
type State = "In Progress" | "In Review" | "Todo" | "Backlog";

const ISSUES: Array<[string, string, Priority, State, string, string]> = [
  ["ENG-142", "Cursor drifts after body convergence", "urgent", "In Progress", "live", "omar"],
  ["ENG-141", "Re-resolve cursors on durable wakeup", "high", "In Review", "live", "omar"],
  ["ENG-138", "Welcome flow founds a Space twice on retry", "high", "Todo", "host", "sam"],
  ["ENG-133", "Milestone scope survives a project switch", "medium", "Todo", "viewer", "kit"],
  ["ENG-129", "Control socket accepts an empty frame", "low", "Backlog", "engine", "sam"],
  ["ENG-121", "Board columns forget collapse state", "low", "Backlog", "viewer", "kit"],
];

const STATE_VARIANT = {
  "In Progress": "yellow",
  "In Review": "purple",
  Todo: "neutral",
  Backlog: "neutral",
} as const;

const row = { display: "flex", alignItems: "center", gap: 12 } as const;

// `<Theme>` syncs `data-theme` onto <html> itself, so setting that attribute
// from outside is overwritten on mount. Scheme is driven through `mode`.
const mode = location.search.includes("light") ? "light" : "dark";

// Density is NOT a theme and does not go through <Theme>. One attribute on the
// root, one style recalc, no React involvement — which is the entire argument
// for the layer. See `tool/generate-astryx-theme.mjs`.
if (location.search.includes("comfortable")) {
  document.documentElement.dataset.density = "comfortable";
}

function Proof() {
  const [seg, setSeg] = useState("compact");
  const [due, setDue] = useState<string | null>(null);
  return (
    <Theme theme={laitTheme} mode={mode}>
      <div style={{ padding: 32, minHeight: "100vh", background: "var(--color-background-body)" }}>
        <div style={{ ...row, justifyContent: "space-between", marginBottom: 20 }}>
          <Heading level={2}>Issues</Heading>
          <div style={row}>
            <Text size="sm" color="secondary">
              Press <Kbd keys="c" /> to create, <Kbd keys="mod+k" /> to search
            </Text>
            <Button variant="secondary" size="sm" label="Filter" />
            <Button variant="primary" size="sm" label="New issue" />
          </div>
        </div>

        <Card>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHeaderCell>Ref</TableHeaderCell>
                <TableHeaderCell>Title</TableHeaderCell>
                <TableHeaderCell>Priority</TableHeaderCell>
                <TableHeaderCell>State</TableHeaderCell>
                <TableHeaderCell>Label</TableHeaderCell>
                <TableHeaderCell>Assignee</TableHeaderCell>
              </TableRow>
            </TableHeader>
            <TableBody>
              {ISSUES.map(([ref, title, priority, state, label, who]) => (
                <TableRow key={ref}>
                  <TableCell>
                    <Text size="sm" color="secondary">
                      {ref}
                    </Text>
                  </TableCell>
                  <TableCell>{title}</TableCell>
                  <TableCell>
                    <span style={{ display: "inline-flex", gap: 6, alignItems: "center" }}>
                      {/* `urgent` is not an Astryx variant — the theme added it. */}
                      <StatusDot variant={priority} label={priority} isPulsing={priority === "urgent"} />
                      <Text size="sm">{priority}</Text>
                    </span>
                  </TableCell>
                  <TableCell>
                    <Badge variant={STATE_VARIANT[state]} label={state} />
                  </TableCell>
                  <TableCell>
                    <Badge variant="teal" label={label} />
                  </TableCell>
                  <TableCell>
                    <Text size="sm" color="secondary">
                      {who}
                    </Text>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </Card>

        <div style={{ marginTop: 28 }}>
          <Text size="sm" color="secondary">
            Priority badges — lait&rsquo;s ramp, entering as generated variants
          </Text>
          <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
            {(["urgent", "high", "medium", "low"] as const).map((v) => (
              <Badge key={v} variant={v} label={v} />
            ))}
          </div>
        </div>

        <div style={{ marginTop: 20 }}>
          <Text size="sm" color="secondary">
            Astryx&rsquo;s nine categorical hues — derived from lait&rsquo;s accent curve
          </Text>
          <div style={{ display: "flex", gap: 8, marginTop: 8, flexWrap: "wrap" }}>
            {(
              ["red", "orange", "yellow", "green", "teal", "cyan", "blue", "purple", "pink", "neutral"] as const
            ).map((v) => (
              <Badge key={v} variant={v} label={v} />
            ))}
          </div>
        </div>

        <div style={{ marginTop: 20 }}>
          <Text size="sm" color="secondary">
            Status
          </Text>
          <div style={{ ...row, marginTop: 8 }}>
            <Badge variant="success" label="Synced" />
            <Badge variant="warning" label="Stale" />
            <Badge variant="error" label="Conflict" />
            <Badge variant="info" label="Draft" />
          </div>
        </div>

        {/* lait's ten button variants, after the collapse. Six of them turned
            out to be components rather than variants. */}
        <div style={{ marginTop: 24 }}>
          <Text size="sm" color="secondary">
            Buttons — lait&rsquo;s ten variants, mapped onto Astryx
          </Text>
          <div style={{ ...row, marginTop: 8, flexWrap: "wrap" }}>
            <Button variant="primary" size="sm" label="Save" />
            <Button variant="secondary" size="sm" label="Outline" elevation="low" />
            <Button variant="secondary" size="sm" label="Toolbar" />
            <Button variant="ghost" size="sm" label="Cancel" />
            {/* `danger` is not an Astryx variant — the theme added it. */}
            <Button variant="danger" size="sm" label="Remove" />
            <Button variant="destructive" size="sm" label="Delete" />
            <Button variant="secondary" size="sm" label="Saving" isLoading />
            <IconButton variant="ghost" size="sm" label="More" icon={<Icon icon="moreHorizontal" />} />
            <Link href="#" isStandalone>Inline action</Link>
          </div>
        </div>

        {/* The regressed row, rebuilt: project tabs plus the toolbar's icon
            buttons. Compare shape (pills, circles) and the selected state. */}
        <div style={{ marginTop: 24 }}>
          <Text size="sm" color="secondary">
            Project tabs — shape and selected state
          </Text>
          <div
            style={{
              marginTop: 8,
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              padding: "8px 4px",
            }}
          >
            <div style={{ display: "flex", gap: 8 }}>
              {(["Overview", "Activity", "Issues", "Specs"] as const).map((tab) => (
                <Button
                  key={tab}
                  size="md"
                  variant={tab === "Issues" ? "active" : "secondary"}
                  elevation={tab === "Issues" ? "none" : "low"}
                  label={tab}
                />
              ))}
            </div>
            <div style={{ display: "flex", gap: 6 }}>
              <IconButton label="Filter" variant="secondary" elevation="low" size="sm" tooltip="Filter" icon={<Icon icon="funnel" />} />
              <IconButton label="Display" variant="secondary" elevation="low" size="sm" tooltip="Display" icon={<Icon icon="wrench" />} />
              <IconButton label="New" variant="secondary" elevation="low" size="sm" tooltip="New" icon={<Icon icon="chevronRight" />} />
              <IconButton label="Panel" variant="active" size="sm" tooltip="Panel" icon={<Icon icon="viewColumns" />} />
            </div>
          </div>
        </div>

        <div style={{ marginTop: 24 }}>
          <Text size="sm" color="secondary">
            Date picker — the quick rows and the month grid
          </Text>
          <div style={{ marginTop: 8 }}>
            <DatePicker value={due} onChange={setDue} />
          </div>
        </div>

        <div style={{ marginTop: 20 }}>
          <Text size="sm" color="secondary">
            Popover — the shape every menu in the app now uses
          </Text>
          <div style={{ marginTop: 8 }}>
            <Popover
              alignment="start"
              content={
                <div style={{ padding: 12, width: 240 }}>
                  <Text size="sm">Local and peer health</Text>
                  <div style={{ marginTop: 8, display: "flex", gap: 8 }}>
                    <Badge variant="success" label="Synced" />
                    <Badge variant="neutral" label="2 peers" />
                  </div>
                </div>
              }
            >
              <Button variant="secondary" size="sm" label="Open popover" />
            </Popover>
          </div>
        </div>

        <div style={{ marginTop: 20 }}>
          <Text size="sm" color="secondary">
            Segmented control — what lait&rsquo;s <code>active</code> variant was emulating
          </Text>
          <div style={{ marginTop: 8, maxWidth: 320 }}>
            <SegmentedControl label="Density" value={seg} onChange={setSeg}>
              <SegmentedControlItem value="compact" label="Compact" />
              <SegmentedControlItem value="comfortable" label="Comfortable" />
            </SegmentedControl>
          </div>
        </div>
      </div>
    </Theme>
  );
}

// Vite re-executes this module on HMR, and a second `createRoot` on the same
// container wedges the renderer rather than erroring cleanly. Cache it.
const container = document.getElementById("root")!;
const w = window as unknown as { __proofRoot?: ReturnType<typeof createRoot> };
(w.__proofRoot ??= createRoot(container)).render(<Proof />);
