import { useLayoutEffect, useRef, useState } from "react";
import { AnimatePresence } from "framer-motion";
import { ContextMenu } from "@base-ui/react/context-menu";
import { useDrag, usePinch } from "@use-gesture/react";
import type { SignageMedia, SignageProgram } from "@/utils/lait/types";
import type { KindDefinition } from "@/utils/apps/api";
import { AddPopover } from "./AddPopover";
import { Clip } from "./Clip";
import { haptic } from "./haptic";
import { OverlayMenu, trackMenuItems, type ClipActions } from "./ItemMenu";
import { useCoarsePointer } from "./pointer";
import {
  ADD_GAP_PX,
  ADD_SLOT_PX,
  CLIP_GAP_PX,
  GRAVITY_PX,
  PX_PER_MS,
  clampZoom,
  formatDuration,
  insertIndexAtX,
  layout,
  playheadX,
  rulerIntervalSec,
  timeAtX,
  type TrimEdge,
  type TrimPreview,
} from "./model";

type Props = {
  program: SignageProgram;
  library: Map<string, SignageMedia>;
  media: SignageMedia[];
  t: number;
  selectedId: string | null;
  orbit: string | null;
  addOpen: boolean;
  wide: boolean;
  container?: React.RefObject<HTMLElement | null>;
  actions: ClipActions;
  onSelect: (id: string | null) => void;
  onSeek: (ms: number) => void;
  onMove: (from: number, to: number) => void;
  onTrim: (itemId: string, durationMs: number) => void;
  onTrimLive: (preview: TrimPreview | null) => void;
  onAddOpenChange: (open: boolean) => void;
  onAddMedia: (media: SignageMedia) => void;
  onUploaded: (media: SignageMedia[]) => void;
  onAddKind: (kind: KindDefinition) => void;
  onUploadError?: (message: string) => void;
};

export function Filmstrip({
  program,
  library,
  media,
  t,
  selectedId,
  orbit,
  addOpen,
  wide,
  container,
  actions,
  onSelect,
  onSeek,
  onMove,
  onTrim,
  onTrimLive,
  onAddOpenChange,
  onAddMedia,
  onUploaded,
  onAddKind,
  onUploadError,
}: Props) {
  const trackRef = useRef<HTMLDivElement>(null);
  const rulerRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const placeRef = useRef<{ at: number; from: number } | null>(null);
  const [place, setPlace] = useState<{ at: number; from: number } | null>(null);
  const [zoom, setZoom] = useState(1);
  const zoomRef = useRef(1);
  const anchorRef = useRef<{ t: number; viewX: number } | null>(null);
  const pxPerMs = PX_PER_MS * zoom;
  const clips = layout(program, library, pxPerMs);
  const clipsRef = useRef(clips);
  const onSeekRef = useRef(onSeek);
  const onMoveRef = useRef(onMove);
  clipsRef.current = clips;
  onSeekRef.current = onSeek;
  onMoveRef.current = onMove;
  zoomRef.current = zoom;

  const clipsWidth =
    clips.length === 0 ? 0 : clips[clips.length - 1].x + clips[clips.length - 1].w;
  const addLeft = clips.length === 0 ? 0 : clipsWidth + ADD_GAP_PX;
  const width = addLeft + ADD_SLOT_PX;
  const head = playheadX(clips, t);
  const lastMs =
    clips.length === 0
      ? 0
      : clips[clips.length - 1].startMs + clips[clips.length - 1].durationMs;

  const xOnTrack = (clientX: number): number => {
    const track = trackRef.current ?? rulerRef.current;
    if (!track) return 0;
    return clientX - track.getBoundingClientRect().left;
  };

  const visibleRange = (): { left: number; right: number } => {
    const scroll = scrollRef.current;
    if (!scroll) return { left: 0, right: width };
    const left = scroll.scrollLeft;
    return { left, right: left + scroll.clientWidth };
  };

  const scootFor = (index: number): number => {
    if (!place) return 0;
    const { at, from } = place;
    if (index === from) return 0;
    if (from < at && index > from && index < at) return -GRAVITY_PX;
    if (from > at && index >= at && index < from) return GRAVITY_PX;
    return 0;
  };

  const clearGravity = () => {
    placeRef.current = null;
    setPlace(null);
  };

  const applyGravity = (clientX: number, from: number) => {
    const laid = clipsRef.current;
    const at = insertIndexAtX(laid, xOnTrack(clientX));
    const next = at === from || at === from + 1 ? null : { at, from };
    const prev = placeRef.current;
    if (prev?.at === next?.at && prev?.from === next?.from) return;
    if (!prev && !next) return;
    placeRef.current = next;
    setPlace(next);
    if (next) {
      const { left, right } = visibleRange();
      const slotX =
        next.at >= laid.length
          ? laid.length === 0
            ? 0
            : laid[laid.length - 1].x + laid[laid.length - 1].w
          : laid[next.at].x;
      if (slotX >= left && slotX <= right) haptic("snap");
    }
  };

  const gapLeft = (() => {
    if (!place) return 0;
    const laid = clips;
    const { at } = place;
    if (at >= laid.length) {
      if (laid.length === 0) return 0;
      return laid[laid.length - 1].x + laid[laid.length - 1].w;
    }
    return Math.max(0, laid[at].x - GRAVITY_PX / 2);
  })();

  const addScoot =
    place && place.at >= clips.length && place.from < place.at
      ? GRAVITY_PX
      : 0;

  const previewTrim = (id: string, nextWidth: number, edge: TrimEdge) => {
    const track = trackRef.current;
    const laid = clipsRef.current;
    if (!track) return;
    const index = laid.findIndex((clip) => clip.item.id === id);
    if (index < 0) return;
    const origin = laid[index];
    let x = 0;
    for (let i = 0; i < laid.length; i++) {
      const clip = laid[i];
      const node = track.querySelector<HTMLElement>(
        `[data-clip-id="${clip.item.id}"]`,
      );
      if (i < index) {
        if (node) node.style.left = `${clip.x}px`;
        x = clip.x + clip.w + CLIP_GAP_PX;
        continue;
      }
      if (i === index) {
        const left =
          edge === "left" ? origin.x + origin.w - nextWidth : origin.x;
        if (node) {
          node.style.left = `${left}px`;
          node.style.width = `${nextWidth}px`;
        }
        x = left + nextWidth;
        if (i < laid.length - 1) x += CLIP_GAP_PX;
        continue;
      }
      if (edge === "left") {
        if (node) node.style.left = `${clip.x}px`;
      } else {
        if (node) node.style.left = `${x}px`;
        x += clip.w;
        if (i < laid.length - 1) x += CLIP_GAP_PX;
      }
    }
    const endX =
      edge === "left"
        ? laid.length === 0
          ? nextWidth
          : laid[laid.length - 1].x + laid[laid.length - 1].w
        : x;
    const addLeftNow = endX + ADD_GAP_PX;
    const next = addLeftNow + ADD_SLOT_PX;
    const add = track.querySelector<HTMLElement>(".pe-add");
    if (add) add.style.left = `${addLeftNow}px`;
    track.style.width = `${next}px`;
    if (rulerRef.current) rulerRef.current.style.width = `${next}px`;
  };

  useLayoutEffect(() => {
    const anchor = anchorRef.current;
    const scroll = scrollRef.current;
    if (!anchor || !scroll) return;
    const x = playheadX(clipsRef.current, anchor.t);
    scroll.scrollLeft = Math.max(0, x - anchor.viewX);
    anchorRef.current = null;
  }, [zoom, clips]);

  useDrag(
    ({ xy: [clientX] }) => {
      onSeekRef.current(timeAtX(clipsRef.current, xOnTrack(clientX)));
    },
    {
      target: rulerRef,
      axis: "x",
      pointer: { keys: false },
      eventOptions: { passive: false },
    },
  );

  usePinch(
    ({ offset: [scale], origin: [ox], first }) => {
      const next = clampZoom(scale);
      const scroll = scrollRef.current;
      if (first && scroll) {
        const viewX = ox - scroll.getBoundingClientRect().left;
        anchorRef.current = {
          t: timeAtX(clipsRef.current, xOnTrack(ox)),
          viewX,
        };
      }
      if (next === zoomRef.current) return;
      if (!anchorRef.current && scroll) {
        const viewX = ox - scroll.getBoundingClientRect().left;
        anchorRef.current = {
          t: timeAtX(clipsRef.current, xOnTrack(ox)),
          viewX,
        };
      }
      setZoom(next);
    },
    {
      target: scrollRef,
      from: () => [zoomRef.current, 0],
      scaleBounds: { min: 0.4, max: 3.5 },
      rubberband: true,
      pinchOnWheel: true,
      modifierKey: "ctrlKey",
      eventOptions: { passive: false },
    },
  );

  const coarsePointer = useCoarsePointer();
  const interval = rulerIntervalSec(pxPerMs);
  const trackBody = (
    <>
      <AnimatePresence initial={false}>
        {clips.map((clip) => (
          <Clip
            key={clip.item.id}
            clip={clip}
            selected={selectedId === clip.item.id}
            orbit={orbit}
            pxPerMs={pxPerMs}
            container={container}
            scroll={scrollRef}
            actions={actions}
            onSelect={onSelect}
            onReorder={(from, clientX) => {
              const to = insertIndexAtX(clipsRef.current, xOnTrack(clientX));
              if (to !== from && to !== from + 1) {
                onMoveRef.current(from, to);
              }
            }}
            onTrim={onTrim}
            onTrimPreview={(id, nextWidth, edge) => {
              previewTrim(id, nextWidth, edge);
              const durationMs = Math.max(
                1000,
                Math.round(nextWidth / (PX_PER_MS * zoomRef.current)),
              );
              onTrimLive({ id, edge, durationMs });
            }}
            onTrimEnd={() => onTrimLive(null)}
            onDragX={applyGravity}
            onDragEnd={clearGravity}
            scoot={scootFor(clip.index)}
          />
        ))}
      </AnimatePresence>
      <AddPopover
        open={addOpen}
        onOpenChange={onAddOpenChange}
        library={media}
        orbit={orbit}
        onAdd={onAddMedia}
        onUploaded={onUploaded}
        onAddKind={onAddKind}
        onUploadError={onUploadError}
        container={container}
        asButton={!wide}
        style={{ left: addLeft + addScoot }}
      />
      {place ? <div className="pe-gap" style={{ left: gapLeft }} /> : null}
      {clips.length > 0 ? (
        <div className="pe-playhead" style={{ left: head }} />
      ) : null}
    </>
  );
  const ticks: { x: number; label: string; ms: number }[] = [];
  for (let sec = 0; sec <= lastMs / 1000 + interval; sec += interval) {
    const ms = sec * 1000;
    const x = playheadX(clips, ms);
    if (clips.length === 0) break;
    const labeled = interval >= 5 || sec % 5 === 0;
    ticks.push({
      x,
      ms,
      label: labeled ? (sec === 0 ? "0" : formatDuration(ms)) : "",
    });
  }

  return (
    <div className="pe-strip-wrap">
      <div className="pe-scroll" ref={scrollRef}>
        <div className="pe-ruler" ref={rulerRef} style={{ width }}>
          {ticks.map((tick) => (
            <span
              key={tick.ms}
              className={`pe-tick${tick.label ? " is-label" : ""}`}
              style={{ left: tick.x }}
            >
              {tick.label || ""}
            </span>
          ))}
          {clips.length > 0 ? (
            <div className="pe-playhead" style={{ left: head }} />
          ) : null}
        </div>
        {coarsePointer ? (
          <div className="pe-track" ref={trackRef} style={{ width }}>
            {trackBody}
          </div>
        ) : (
          <ContextMenu.Root>
            <ContextMenu.Trigger
              render={
                <div className="pe-track" ref={trackRef} style={{ width }} />
              }
            >
              {trackBody}
            </ContextMenu.Trigger>
            <OverlayMenu items={trackMenuItems(actions)} container={container} />
          </ContextMenu.Root>
        )}
      </div>
    </div>
  );
}
