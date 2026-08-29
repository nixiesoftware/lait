import { useEffect, useRef, useState } from "react";
import { motion } from "framer-motion";
import { ContextMenu } from "@base-ui/react/context-menu";
import interact from "interactjs";
import { layoutTransition, overlayTransition, presence } from "@/ds";
import { haptic } from "./haptic";
import {
  durationForWidth,
  formatDuration,
  HOLD_MS,
  MIN_CLIP_PX,
  nativeDurationMs,
  widthForDuration,
  type LaidClip,
  type TrimEdge,
} from "./model";
import { OverlayMenu, clipMenuItems, type ClipActions } from "./ItemMenu";
import { useCoarsePointer } from "./pointer";
import { Thumb } from "./Thumb";

type Props = {
  clip: LaidClip;
  selected: boolean;
  orbit: string | null;
  pxPerMs: number;
  container?: React.RefObject<HTMLElement | null>;
  scroll?: React.RefObject<HTMLElement | null>;
  actions: ClipActions;
  onSelect: (id: string) => void;
  onReorder: (from: number, clientX: number) => void;
  onTrim: (id: string, durationMs: number) => void;
  onTrimPreview: (id: string, width: number, edge: TrimEdge) => void;
  onTrimEnd: () => void;
  onDragX: (clientX: number, from: number) => void;
  onDragEnd: () => void;
  scoot?: number;
};

export function Clip({
  clip,
  selected,
  orbit,
  pxPerMs,
  container,
  scroll,
  actions,
  onSelect,
  onReorder,
  onTrim,
  onTrimPreview,
  onTrimEnd,
  onDragX,
  onDragEnd,
  scoot = 0,
}: Props) {
  const slotRef = useRef<HTMLDivElement>(null);
  const elRef = useRef<HTMLDivElement>(null);
  const [gesturing, setGesturing] = useState(false);
  const [lifting, setLifting] = useState(false);
  const clipRef = useRef(clip);
  const pxRef = useRef(pxPerMs);
  const onSelectRef = useRef(onSelect);
  const onReorderRef = useRef(onReorder);
  const onTrimRef = useRef(onTrim);
  const onTrimPreviewRef = useRef(onTrimPreview);
  const onTrimEndRef = useRef(onTrimEnd);
  const onDragXRef = useRef(onDragX);
  const onDragEndRef = useRef(onDragEnd);
  const scrollRef = useRef(scroll);
  clipRef.current = clip;
  pxRef.current = pxPerMs;
  onSelectRef.current = onSelect;
  onReorderRef.current = onReorder;
  onTrimRef.current = onTrim;
  onTrimPreviewRef.current = onTrimPreview;
  onTrimEndRef.current = onTrimEnd;
  onDragXRef.current = onDragX;
  onDragEndRef.current = onDragEnd;
  scrollRef.current = scroll;

  useEffect(() => {
    const slot = slotRef.current;
    const el = elRef.current;
    if (!slot || !el) return;
    const resize = interact(slot);
    const drag = interact(el);
    resize.styleCursor(false);
    drag.styleCursor(false);
    const scrollEl = scrollRef.current?.current ?? undefined;
    const coarse = window.matchMedia("(pointer: coarse)").matches;

    drag.draggable({
      inertia: false,
      hold: coarse ? HOLD_MS : 0,
      mouseButtons: 1,
      startAxis: "x",
      lockAxis: "x",
      ignoreFrom: ".pe-handle",
      autoScroll: scrollEl
        ? { container: scrollEl, margin: 48, speed: 420 }
        : false,
      listeners: {
        start() {
          setGesturing(true);
          setLifting(true);
          onSelectRef.current(clipRef.current.item.id);
          haptic("lift");
        },
        move(event) {
          const x = (parseFloat(el.dataset.dx || "0") || 0) + event.dx;
          el.dataset.dx = String(x);
          el.style.transform = `translateX(${x}px) scale(1.04)`;
          onDragXRef.current(event.clientX, clipRef.current.index);
        },
        end(event) {
          const x = parseFloat(el.dataset.dx || "0") || 0;
          el.dataset.dx = "0";
          el.style.transform = "";
          setLifting(false);
          setGesturing(false);
          onDragEndRef.current();
          if (Math.abs(x) < 6) return;
          onReorderRef.current(clipRef.current.index, event.clientX);
        },
      },
    });

    resize.resizable({
      edges: { left: ".pe-handle-start", right: ".pe-handle-end", top: false, bottom: false },
      allowFrom: ".pe-handle",
      margin: 0,
      inertia: false,
      modifiers: [
        interact.modifiers.restrictSize({
          min: { width: MIN_CLIP_PX, height: 1 },
        }),
      ],
      listeners: {
        start() {
          setGesturing(true);
          onSelectRef.current(clipRef.current.item.id);
          slot.dataset.trimAtMin = "0";
          slot.dataset.trimAtNative = "0";
        },
        move(event) {
          const w = Math.max(MIN_CLIP_PX, event.rect.width);
          slot.style.height = "";
          slot.style.transform = "";
          const edge: TrimEdge = event.edges.left ? "left" : "right";
          const label = el.querySelector("[data-clip-clock]");
          if (label) {
            label.textContent = formatDuration(durationForWidth(w, pxRef.current));
          }
          const native = nativeDurationMs(clipRef.current.media);
          const nativeW =
            native != null ? widthForDuration(native, pxRef.current) : null;
          const atMin = w <= MIN_CLIP_PX + 0.5;
          const atNative = nativeW != null && Math.abs(w - nativeW) < 3;
          if (atMin && slot.dataset.trimAtMin !== "1") haptic("edge");
          if (atNative && slot.dataset.trimAtNative !== "1") haptic("edge");
          slot.dataset.trimAtMin = atMin ? "1" : "0";
          slot.dataset.trimAtNative = atNative ? "1" : "0";
          onTrimPreviewRef.current(clipRef.current.item.id, w, edge);
        },
        end(event) {
          setGesturing(false);
          onTrimRef.current(
            clipRef.current.item.id,
            durationForWidth(event.rect.width, pxRef.current),
          );
          onTrimEndRef.current();
        },
      },
    });
    return () => {
      drag.unset();
      resize.unset();
    };
  }, []);

  const nativeMs = nativeDurationMs(clip.media);
  const ghostW =
    nativeMs != null ? widthForDuration(nativeMs, pxPerMs) : null;
  const coarse = useCoarsePointer();
  const clipBody = (
    <>
      <div className="pe-clip-fill">
        <Thumb media={clip.media} orbit={orbit} />
      </div>
      <div className="pe-clip-body">
        <strong>{clip.media?.name ?? clip.item.media}</strong>
        <span data-clip-clock>{formatDuration(clip.durationMs)}</span>
      </div>
      {selected ? (
        <>
          <button
            type="button"
            className="pe-handle pe-handle-start"
            aria-label="Trim start"
          />
          <button
            type="button"
            className="pe-handle pe-handle-end"
            aria-label="Trim end"
          />
        </>
      ) : null}
    </>
  );

  const slot = (
    <motion.div
      ref={slotRef}
      className="pe-clip-slot"
      data-clip-id={clip.item.id}
      style={{ left: clip.x + scoot, width: clip.w }}
      layout={!gesturing}
      initial={presence.initial}
      animate={presence.animate}
      exit={presence.exit}
      transition={
        gesturing
          ? { duration: 0 }
          : { layout: layoutTransition, opacity: overlayTransition }
      }
    >
      {ghostW != null && selected ? (
        <div className="pe-clip-ghost" style={{ width: ghostW }} aria-hidden />
      ) : null}
      {coarse ? (
        <div
          ref={elRef}
          className={`pe-clip${selected ? " is-selected" : ""}${lifting ? " is-lifting is-moving" : ""}`}
          onPointerDown={() => onSelect(clip.item.id)}
        >
          {clipBody}
        </div>
      ) : (
        <ContextMenu.Trigger
          render={
            <div
              ref={elRef}
              className={`pe-clip${selected ? " is-selected" : ""}${lifting ? " is-lifting is-moving" : ""}`}
              onPointerDown={() => onSelect(clip.item.id)}
            />
          }
        >
          {clipBody}
        </ContextMenu.Trigger>
      )}
    </motion.div>
  );

  if (coarse) return slot;

  return (
    <ContextMenu.Root>
      {slot}
      <OverlayMenu items={clipMenuItems(clip, actions)} container={container} />
    </ContextMenu.Root>
  );
}
