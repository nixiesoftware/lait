/**
 * Utilities for processing colors extracted by color-thief-react
 */

export interface ColorInfo {
  dominant: string;
  secondary: string;
  accent: string;
}

/**
 * Convert RGB array to hex color
 */
function rgbToHex(r: number, g: number, b: number): string {
  return '#' + [r, g, b].map(x => {
    const hex = Math.round(x).toString(16);
    return hex.length === 1 ? '0' + hex : hex;
  }).join('');
}

function hexToRgb(hex: string): [number, number, number] {
  const clean = hex.replace('#', '');
  const bigint = parseInt(clean.length === 3 ? clean.split('').map(c => c + c).join('') : clean, 16);
  const r = (bigint >> 16) & 255;
  const g = (bigint >> 8) & 255;
  const b = bigint & 255;
  return [r, g, b];
}

function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
  r /= 255; g /= 255; b /= 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b);
  let h = 0, s = 0; const l = (max + min) / 2;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    switch (max) {
      case r: h = (g - b) / d + (g < b ? 6 : 0); break;
      case g: h = (b - r) / d + 2; break;
      case b: h = (r - g) / d + 4; break;
    }
    h /= 6;
  }
  return [h * 360, s, l];
}

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
  h /= 360;
  let r: number, g: number, b: number;
  if (s === 0) {
    r = g = b = l; // achromatic
  } else {
    const hue2rgb = (p: number, q: number, t: number) => {
      if (t < 0) t += 1;
      if (t > 1) t -= 1;
      if (t < 1 / 6) return p + (q - p) * 6 * t;
      if (t < 1 / 2) return q;
      if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
      return p;
    };
    const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
    const p = 2 * l - q;
    r = hue2rgb(p, q, h + 1 / 3);
    g = hue2rgb(p, q, h);
    b = hue2rgb(p, q, h - 1 / 3);
  }
  return [Math.round(r * 255), Math.round(g * 255), Math.round(b * 255)];
}

function clamp(n: number, min: number, max: number) { return Math.max(min, Math.min(max, n)); }
function hueDistance(a: number, b: number) {
  const d = Math.abs(a - b) % 360; return d > 180 ? 360 - d : d;
}

function ensureVibrant(hex: string, opts?: { minS?: number; targetL?: number; boost?: number }): string {
  const [r, g, b] = hexToRgb(hex);
  const [h, s0, l0] = rgbToHsl(r, g, b);
  const minS = opts?.minS ?? 0.55;     // at least this saturation
  const targetL = opts?.targetL ?? 0.45; // aim mid brightness for body bg
  const boost = opts?.boost ?? 1.0;     // multiplicative boost of saturation
  const s = clamp(s0 * boost, minS, 1);
  // Nudge lightness toward target to avoid muddy extremes
  const l = clamp(l0 * 0.6 + targetL * 0.4, 0.25, 0.7);
  const [nr, ng, nb] = hslToRgb(h, s, l);
  return rgbToHex(nr, ng, nb);
}

/**
 * Process color palette from color-thief-react
 * The library returns colors as arrays: [[r,g,b], [r,g,b], ...]
 * We boost saturation and normalize lightness to make gradients vibrant
 * even if the sampled colors are weak.
 */
export function processColorPalette(palette: number[][] | null | undefined): ColorInfo {
  // Default fallback colors
  const defaultColors: ColorInfo = {
    dominant: '#1a1a1a',
    secondary: '#2a2a2a',
    accent: '#3a3a3a'
  };

  if (!palette || palette.length === 0) {
    return defaultColors;
  }

  // Convert to HSL and sort by saturation (desc) to avoid picking muddy colors first
  const hsl = palette.map(([r, g, b]) => {
    const [h, s, l] = rgbToHsl(r, g, b);
    return { h, s, l, rgb: [r, g, b] as [number, number, number] };
  }).sort((a, b) => b.s - a.s);

  const pick = (idx: number) => {
    const c = hsl[idx] ?? hsl[hsl.length - 1];
    const [rr, gg, bb] = hslToRgb(c.h, Math.max(c.s, 0.4), clamp(c.l, 0.25, 0.7));
    return rgbToHex(rr, gg, bb);
  };

  // Initial picks by saturation
  let dominant = pick(0);
  // Try to ensure hue separation for secondary and accent
  let secondary = pick(1);
  let accent = pick(2);

  // Adjust hues if too close: rotate secondary slightly and accent toward complementary region
  const [dr, dg, db] = hexToRgb(dominant);
  const [dh] = rgbToHsl(dr, dg, db);

  const rotateHex = (hex: string, rotate: number, satBoost = 1, targetL?: number) => {
    const [r, g, b] = hexToRgb(hex);
    let [h, s, l] = rgbToHsl(r, g, b);
    h = (h + rotate + 360) % 360;
    s = clamp(s * satBoost, 0.55, 1);
    if (typeof targetL === 'number') l = clamp(targetL, 0.25, 0.7);
    const [nr, ng, nb] = hslToRgb(h, s, l);
    return rgbToHex(nr, ng, nb);
  };

  // If secondary is too close in hue to dominant, rotate 30-50 degrees
  const [sr, sg, sb] = hexToRgb(secondary);
  const [sh] = rgbToHsl(sr, sg, sb);
  if (hueDistance(sh, dh) < 18) {
    secondary = rotateHex(secondary, 40, 1.05);
  }

  // Accent: aim for strong contrast (near complementary)
  const [ar, ag, ab] = hexToRgb(accent);
  const [ah] = rgbToHsl(ar, ag, ab);
  if (hueDistance(ah, dh) < 100) {
    accent = rotateHex(dominant, 180, 1.1, 0.5);
  } else {
    accent = ensureVibrant(accent, { minS: 0.6, targetL: 0.5, boost: 1.15 });
  }

  // Final vibrancy pass to guarantee minimum saturation
  dominant = ensureVibrant(dominant, { minS: 0.55, targetL: 0.42, boost: 1.1 });
  secondary = ensureVibrant(secondary, { minS: 0.55, targetL: 0.46, boost: 1.05 });

  return { dominant, secondary, accent };
}

/**
 * Process a single color from color-thief-react
 * The library returns a single color as [r,g,b] array
 */
export function processSingleColor(color: number[] | string | null | undefined): string {
  if (!color) return '#1a1a1a';

  // If it's already a hex string, return it
  if (typeof color === 'string') return color;

  // Convert RGB array to hex
  const [r, g, b] = color;
  return rgbToHex(r, g, b);
}

// Generate a CSS gradient from extracted colors
export function generateGradient(colors: ColorInfo): string {
  return `linear-gradient(135deg,
    ${colors.dominant}15 0%,
    ${colors.secondary}20 50%,
    ${colors.accent}15 100%)`;
}

// Generate a more vibrant gradient for dark mode/editor backdrops
export function generateVibrantGradient(colors: ColorInfo): string {
  // Slightly adjust lightness per stop to add depth without dulling
const withAlpha = (hex: string, alpha: number) => {
    const a = Math.round(clamp(alpha, 0, 1) * 255).toString(16).padStart(2, '0');
    return hex + a;
  };

  return `linear-gradient(135deg,
    ${withAlpha(colors.dominant, 0.95)} 0%,
    ${withAlpha(colors.secondary, 0.92)} 52%,
    ${withAlpha(colors.accent, 0.95)} 100%),
    radial-gradient(60% 80% at 20% 10%, ${withAlpha(colors.accent, 0.18)}, transparent 70%),
    radial-gradient(80% 60% at 80% 90%, ${withAlpha(colors.secondary, 0.14)}, transparent 70%)`;
}

// Derive a quick, vibrant triad from a single base color (hex)
function triadFromBase(hex: string): ColorInfo {
  const [r, g, b] = hexToRgb(hex);
  const [h, s, l] = rgbToHsl(r, g, b);
  const base = ensureVibrant(hex, { minS: 0.6, targetL: 0.45, boost: 1.1 });
  const rotate = (deg: number, satBoost = 1.05, light?: number) => {
    const nh = (h + deg + 360) % 360;
    const ns = clamp(s * satBoost, 0.55, 1);
    const nl = typeof light === 'number' ? clamp(light, 0.25, 0.7) : l;
    const [rr, gg, bb] = hslToRgb(nh, ns, nl);
    return rgbToHex(rr, gg, bb);
  };
  const secondary = rotate(38, 1.05, 0.46);
  const accent = rotate(180, 1.1, 0.5);
  return { dominant: base, secondary, accent };
}

// Compute an average color from ImageData quickly
function averageHexFromImageData(image: ImageData): string {
  const { data, width, height } = image;
  let r = 0, g = 0, b = 0, count = 0;
  const step = Math.max(1, Math.floor((width * height) / 5000)); // sample up to ~5k pixels
  for (let i = 0; i < width * height; i += step) {
    const idx = i * 4;
    const a = data[idx + 3];
    if (a < 16) continue; // skip near-transparent
    r += data[idx];
    g += data[idx + 1];
    b += data[idx + 2];
    count++;
  }
  if (count === 0) return '#1a1a1a';
  return rgbToHex(r / count, g / count, b / count);
}

// Create an optimistic ColorInfo from a video frame ImageData
export function optimisticPaletteFromImageData(image: ImageData): ColorInfo {
  const avg = averageHexFromImageData(image);
  return triadFromBase(avg);
}
