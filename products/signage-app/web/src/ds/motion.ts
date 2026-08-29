import type { Transition } from "framer-motion";

export const easeOut = [0.22, 1, 0.36, 1] as const;

export const overlayTransition: Transition = {
  duration: 0.16,
  ease: easeOut,
};

export const layoutTransition: Transition = {
  type: "spring",
  stiffness: 520,
  damping: 42,
  mass: 0.8,
};

export const presence = {
  initial: { opacity: 0 },
  animate: { opacity: 1 },
  exit: { opacity: 0 },
};
