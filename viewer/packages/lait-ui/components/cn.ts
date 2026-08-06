import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/**
 * The package owns its class merger rather than importing the viewer's.
 *
 * An integration is consumed from `node_modules`, so a reach back into the app
 * that happens to contain it today is a dependency that only resolves by
 * accident of the workspace layout.
 */
export function cn(...parts: ClassValue[]): string {
  return twMerge(clsx(parts));
}
