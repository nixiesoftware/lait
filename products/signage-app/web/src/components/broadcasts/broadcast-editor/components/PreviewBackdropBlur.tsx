import React from "react";
import type { SignageMedia } from "@/utils/lait/types";

interface PreviewBackdropBlurProps {
  content: SignageMedia | null;
}

export default function PreviewBackdropBlur({ content }: PreviewBackdropBlurProps) {
  // No browser URL exists for stored bytes, so the backdrop is a gradient;
  // a card lends its own background color.
  const cardBackground = content?.source === 'card' ? `#${content.background}` : null;

  const style: React.CSSProperties = cardBackground
    ? {
        backgroundImage: `linear-gradient(to bottom, rgba(17,24,39,0.55) 0%, rgba(17,24,39,0.35) 25%, rgba(17,24,39,0.25) 55%, rgba(17,24,39,0.45) 100%)`,
        backgroundColor: cardBackground,
        backgroundSize: "cover",
        backgroundPosition: "center 30%",
        backgroundRepeat: "no-repeat",
        transform: "scale(1.03)",
        filter: "blur(14px) brightness(0.62) saturate(1.08) contrast(1.05)",
        transition: "background-color 200ms ease, filter 200ms ease, transform 200ms ease",
        willChange: "filter, transform",
      }
    : {
        // Elegant dark-friendly fallback gradient
        backgroundImage: "radial-gradient(120% 80% at 50% 0%, rgba(17,24,39,0.75) 0%, rgba(17,24,39,0.65) 35%, rgba(2,6,23,0.9) 100%)",
        backgroundSize: "cover",
        backgroundPosition: "center",
        transform: "scale(1.02)",
        filter: "blur(18px) brightness(0.7) contrast(1.05)",
        transition: "filter 200ms ease, transform 200ms ease",
        willChange: "filter, transform",
      };

  return (
    <div
      className="absolute inset-0 -z-10 mx-auto my-auto pointer-events-none aspect-video h-full opacity-80 sm:max-h-[54vh] max-sm:h-auto"
      style={style}
      aria-hidden
    />
  );
}
