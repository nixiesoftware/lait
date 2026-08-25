import { useCallback, useEffect, useRef, useState } from "react";
import type { ProgramCycle } from "@/utils/lait/types";

export function useTransport(durationMs: number, cycle: ProgramCycle) {
  const [t, setT] = useState(0);
  const [playing, setPlaying] = useState(false);
  const playingRef = useRef(false);
  const tRef = useRef(0);
  const lastRef = useRef(0);
  const durationRef = useRef(durationMs);
  const cycleRef = useRef(cycle);
  durationRef.current = durationMs;
  cycleRef.current = cycle;

  useEffect(() => {
    if (durationMs <= 0) {
      tRef.current = 0;
      setT(0);
      return;
    }
    if (tRef.current > durationMs) {
      tRef.current = durationMs;
      setT(durationMs);
    }
  }, [durationMs]);

  useEffect(() => {
    if (!playing) return;
    let frame = 0;
    lastRef.current = performance.now();
    const tick = (now: number) => {
      const dt = now - lastRef.current;
      lastRef.current = now;
      const dur = durationRef.current;
      let next = tRef.current + dt;
      if (dur <= 0) {
        next = 0;
        playingRef.current = false;
        setPlaying(false);
      } else if (next >= dur) {
        if (cycleRef.current === "loop") {
          next = next % dur;
        } else {
          next = dur;
          playingRef.current = false;
          setPlaying(false);
        }
      }
      tRef.current = next;
      setT(next);
      if (playingRef.current) frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [playing]);

  const seek = useCallback((ms: number) => {
    const dur = durationRef.current;
    const next = dur <= 0 ? 0 : Math.max(0, Math.min(dur, ms));
    tRef.current = next;
    setT(next);
  }, []);

  const play = useCallback(() => {
    if (durationRef.current <= 0) return;
    if (tRef.current >= durationRef.current) {
      tRef.current = 0;
      setT(0);
    }
    playingRef.current = true;
    setPlaying(true);
  }, []);

  const pause = useCallback(() => {
    playingRef.current = false;
    setPlaying(false);
  }, []);

  const toggle = useCallback(() => {
    if (playingRef.current) pause();
    else play();
  }, [pause, play]);

  return { t, playing, seek, play, pause, toggle };
}
