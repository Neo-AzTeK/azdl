// Material You (Material Design 3) Color & Theming System for azdl

export const PRESET_SEEDS = [
  { name: 'Emerald', hex: '#10B981', label: 'Pixel Emerald' },
  { name: 'Blue', hex: '#2563EB', label: 'Ocean Blue' },
  { name: 'Violet', hex: '#7C3AED', label: 'Coral Violet' },
  { name: 'Amber', hex: '#F59E0B', label: 'Cyberpunk Amber' },
  { name: 'Rose', hex: '#F43F5E', label: 'Neon Rose' },
  { name: 'Cyan', hex: '#06B6D4', label: 'Electric Cyan' },
  { name: 'Purple', hex: '#9333EA', label: 'Deep Amethyst' },
];

// Helper: Hex to RGB
export function hexToRgb(hex) {
  let c = hex.replace(/^#/, '');
  if (c.length === 3) {
    c = c.split('').map((x) => x + x).join('');
  }
  const num = parseInt(c, 16);
  return {
    r: (num >> 16) & 255,
    g: (num >> 8) & 255,
    b: num & 255,
  };
}

// Helper: RGB to Hex
export function rgbToHex(r, g, b) {
  const clamp = (v) => Math.max(0, Math.min(255, Math.round(v)));
  return (
    '#' +
    [clamp(r), clamp(g), clamp(b)]
      .map((x) => x.toString(16).padStart(2, '0'))
      .join('')
  );
}

// Helper: RGB to HSL
export function rgbToHsl(r, g, b) {
  r /= 255;
  g /= 255;
  b /= 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  let h = 0,
    s = 0,
    l = (max + min) / 2;

  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    switch (max) {
      case r:
        h = (g - b) / d + (g < b ? 6 : 0);
        break;
      case g:
        h = (b - r) / d + 2;
        break;
      case b:
        h = (r - g) / d + 4;
        break;
    }
    h /= 6;
  }
  return { h: h * 360, s, l };
}

// Helper: HSL to RGB
export function hslToRgb(h, s, l) {
  h = (h % 360 + 360) % 360 / 360;
  let r, g, b;

  if (s === 0) {
    r = g = b = l;
  } else {
    const hue2rgb = (p, q, t) => {
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
  return {
    r: Math.round(r * 255),
    g: Math.round(g * 255),
    b: Math.round(b * 255),
  };
}

// Tone generator from HSL (mimicking Material 3 HCT tonal palettes)
function toneToRgb(hue, chroma, tone) {
  const l = tone / 100;
  // Saturated for tone 40-80, more subtle elsewhere
  const s = Math.min(1, chroma * (1 - Math.abs(l - 0.5) * 0.8));
  return hslToRgb(hue, s, l);
}

// Helper: Resolve 'system' mode to actual 'dark' or 'light'
export function resolveThemeMode(mode = 'dark') {
  if (mode === 'system') {
    if (typeof window !== 'undefined' && window.matchMedia) {
      return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
    }
    return 'dark';
  }
  return mode;
}

// Helper: Listen for OS dark/light mode changes
export function onSystemThemeChange(callback) {
  if (typeof window === 'undefined' || !window.matchMedia) {
    return () => {};
  }
  const mediaQuery = window.matchMedia('(prefers-color-scheme: dark)');
  const listener = (e) => {
    callback(e.matches ? 'dark' : 'light');
  };
  if (mediaQuery.addEventListener) {
    mediaQuery.addEventListener('change', listener);
    return () => mediaQuery.removeEventListener('change', listener);
  } else if (mediaQuery.addListener) {
    mediaQuery.addListener(listener);
    return () => mediaQuery.removeListener(listener);
  }
  return () => {};
}

export function generateMaterialPalette(seedHex, mode = 'dark') {
  const effectiveMode = resolveThemeMode(mode);
  const rgb = hexToRgb(seedHex);
  const hsl = rgbToHsl(rgb.r, rgb.g, rgb.b);
  const hue = hsl.h;
  const chroma = Math.max(0.4, Math.min(0.9, hsl.s));

  const isDark = effectiveMode === 'dark' || effectiveMode === 'amoled';
  const isAmoled = effectiveMode === 'amoled';

  // Primary tones
  const primaryTone = isDark ? 80 : 40;
  const onPrimaryTone = isDark ? 20 : 100;
  const primaryContainerTone = isDark ? 30 : 90;
  const onPrimaryContainerTone = isDark ? 90 : 10;

  // Secondary tones
  const secondaryHue = (hue + 15) % 360;
  const secondaryChroma = chroma * 0.5;
  const secondaryTone = isDark ? 80 : 40;
  const onSecondaryTone = isDark ? 20 : 100;
  const secondaryContainerTone = isDark ? 30 : 90;
  const onSecondaryContainerTone = isDark ? 90 : 10;

  // Surface & neutral tones with authentic Material You tinting
  const neutralChroma = 0.16;

  const primaryRgb = toneToRgb(hue, chroma, primaryTone);
  const onPrimaryRgb = toneToRgb(hue, chroma, onPrimaryTone);
  const primaryContainerRgb = toneToRgb(hue, chroma, primaryContainerTone);
  const onPrimaryContainerRgb = toneToRgb(hue, chroma, onPrimaryContainerTone);

  const secondaryRgb = toneToRgb(secondaryHue, secondaryChroma, secondaryTone);
  const onSecondaryRgb = toneToRgb(secondaryHue, secondaryChroma, onSecondaryTone);
  const secondaryContainerRgb = toneToRgb(secondaryHue, secondaryChroma, secondaryContainerTone);
  const onSecondaryContainerRgb = toneToRgb(secondaryHue, secondaryChroma, onSecondaryContainerTone);

  // Tertiary tones
  const tertiaryHue = (hue + 60) % 360;
  const tertiaryChroma = chroma * 0.6;
  const tertiaryTone = isDark ? 80 : 40;
  const onTertiaryTone = isDark ? 20 : 100;
  const tertiaryContainerTone = isDark ? 30 : 90;
  const onTertiaryContainerTone = isDark ? 90 : 10;

  const tertiaryRgb = toneToRgb(tertiaryHue, tertiaryChroma, tertiaryTone);
  const onTertiaryRgb = toneToRgb(tertiaryHue, tertiaryChroma, onTertiaryTone);
  const tertiaryContainerRgb = toneToRgb(tertiaryHue, tertiaryChroma, tertiaryContainerTone);
  const onTertiaryContainerRgb = toneToRgb(tertiaryHue, tertiaryChroma, onTertiaryContainerTone);

  let surfaceRgb,
    surfaceDimRgb,
    surfaceBrightRgb,
    surfaceContainerLowestRgb,
    surfaceContainerLowRgb,
    surfaceContainerRgb,
    surfaceContainerHighRgb,
    surfaceContainerHighestRgb,
    onSurfaceRgb,
    onSurfaceVariantRgb,
    outlineRgb,
    outlineVariantRgb;

  if (isAmoled) {
    // Pure AMOLED black mode
    surfaceRgb = { r: 0, g: 0, b: 0 };
    surfaceDimRgb = { r: 0, g: 0, b: 0 };
    surfaceBrightRgb = { r: 24, g: 24, b: 24 };
    surfaceContainerLowestRgb = { r: 0, g: 0, b: 0 };
    surfaceContainerLowRgb = { r: 10, g: 10, b: 12 };
    surfaceContainerRgb = { r: 16, g: 16, b: 18 };
    surfaceContainerHighRgb = { r: 24, g: 24, b: 28 };
    surfaceContainerHighestRgb = { r: 32, g: 32, b: 38 };
    onSurfaceRgb = { r: 240, g: 240, b: 245 };
    onSurfaceVariantRgb = { r: 180, g: 180, b: 190 };
    outlineRgb = { r: 70, g: 70, b: 80 };
    outlineVariantRgb = { r: 35, g: 35, b: 42 };
  } else if (isDark) {
    surfaceRgb = toneToRgb(hue, neutralChroma, 10);
    surfaceDimRgb = toneToRgb(hue, neutralChroma, 8);
    surfaceBrightRgb = toneToRgb(hue, neutralChroma, 24);
    surfaceContainerLowestRgb = toneToRgb(hue, neutralChroma, 6);
    surfaceContainerLowRgb = toneToRgb(hue, neutralChroma, 12);
    surfaceContainerRgb = toneToRgb(hue, neutralChroma, 16);
    surfaceContainerHighRgb = toneToRgb(hue, neutralChroma, 22);
    surfaceContainerHighestRgb = toneToRgb(hue, neutralChroma, 28);
    onSurfaceRgb = toneToRgb(hue, neutralChroma, 92);
    onSurfaceVariantRgb = toneToRgb(hue, neutralChroma, 82);
    outlineRgb = toneToRgb(hue, neutralChroma, 60);
    outlineVariantRgb = toneToRgb(hue, neutralChroma, 32);
  } else {
    // Light mode
    surfaceRgb = toneToRgb(hue, neutralChroma, 98);
    surfaceDimRgb = toneToRgb(hue, neutralChroma, 87);
    surfaceBrightRgb = toneToRgb(hue, neutralChroma, 98);
    surfaceContainerLowestRgb = toneToRgb(hue, neutralChroma, 100);
    surfaceContainerLowRgb = toneToRgb(hue, neutralChroma, 96);
    surfaceContainerRgb = toneToRgb(hue, neutralChroma, 94);
    surfaceContainerHighRgb = toneToRgb(hue, neutralChroma, 92);
    surfaceContainerHighestRgb = toneToRgb(hue, neutralChroma, 90);
    onSurfaceRgb = toneToRgb(hue, neutralChroma, 10);
    onSurfaceVariantRgb = toneToRgb(hue, neutralChroma, 30);
    outlineRgb = toneToRgb(hue, neutralChroma, 50);
    outlineVariantRgb = toneToRgb(hue, neutralChroma, 80);
  }

  return {
    primary: rgbToHex(primaryRgb.r, primaryRgb.g, primaryRgb.b),
    primaryRgb,
    onPrimary: rgbToHex(onPrimaryRgb.r, onPrimaryRgb.g, onPrimaryRgb.b),
    primaryContainer: rgbToHex(primaryContainerRgb.r, primaryContainerRgb.g, primaryContainerRgb.b),
    onPrimaryContainer: rgbToHex(onPrimaryContainerRgb.r, onPrimaryContainerRgb.g, onPrimaryContainerRgb.b),

    secondary: rgbToHex(secondaryRgb.r, secondaryRgb.g, secondaryRgb.b),
    onSecondary: rgbToHex(onSecondaryRgb.r, onSecondaryRgb.g, onSecondaryRgb.b),
    secondaryContainer: rgbToHex(secondaryContainerRgb.r, secondaryContainerRgb.g, secondaryContainerRgb.b),
    onSecondaryContainer: rgbToHex(onSecondaryContainerRgb.r, onSecondaryContainerRgb.g, onSecondaryContainerRgb.b),

    tertiary: rgbToHex(tertiaryRgb.r, tertiaryRgb.g, tertiaryRgb.b),
    tertiaryRgb,
    onTertiary: rgbToHex(onTertiaryRgb.r, onTertiaryRgb.g, onTertiaryRgb.b),
    tertiaryContainer: rgbToHex(tertiaryContainerRgb.r, tertiaryContainerRgb.g, tertiaryContainerRgb.b),
    onTertiaryContainer: rgbToHex(onTertiaryContainerRgb.r, onTertiaryContainerRgb.g, onTertiaryContainerRgb.b),

    surface: rgbToHex(surfaceRgb.r, surfaceRgb.g, surfaceRgb.b),
    surfaceRgb,
    surfaceDim: rgbToHex(surfaceDimRgb.r, surfaceDimRgb.g, surfaceDimRgb.b),
    surfaceBright: rgbToHex(surfaceBrightRgb.r, surfaceBrightRgb.g, surfaceBrightRgb.b),
    surfaceContainerLowest: rgbToHex(surfaceContainerLowestRgb.r, surfaceContainerLowestRgb.g, surfaceContainerLowestRgb.b),
    surfaceContainerLow: rgbToHex(surfaceContainerLowRgb.r, surfaceContainerLowRgb.g, surfaceContainerLowRgb.b),
    surfaceContainer: rgbToHex(surfaceContainerRgb.r, surfaceContainerRgb.g, surfaceContainerRgb.b),
    surfaceContainerHigh: rgbToHex(surfaceContainerHighRgb.r, surfaceContainerHighRgb.g, surfaceContainerHighRgb.b),
    surfaceContainerHighest: rgbToHex(surfaceContainerHighestRgb.r, surfaceContainerHighestRgb.g, surfaceContainerHighestRgb.b),

    onSurface: rgbToHex(onSurfaceRgb.r, onSurfaceRgb.g, onSurfaceRgb.b),
    onSurfaceVariant: rgbToHex(onSurfaceVariantRgb.r, onSurfaceVariantRgb.g, onSurfaceVariantRgb.b),
    outline: rgbToHex(outlineRgb.r, outlineRgb.g, outlineRgb.b),
    outlineVariant: rgbToHex(outlineVariantRgb.r, outlineVariantRgb.g, outlineVariantRgb.b),
  };
}

export function applyTheme(seedHex, mode = 'dark', translucency = 'none') {
  const effectiveMode = resolveThemeMode(mode);
  const p = generateMaterialPalette(seedHex, effectiveMode);
  const root = document.documentElement;

  // Material Design 3 and m3-svelte tokens
  root.style.setProperty('--md-sys-color-primary', p.primary);
  root.style.setProperty('--md-sys-color-primary-rgb', `${p.primaryRgb.r}, ${p.primaryRgb.g}, ${p.primaryRgb.b}`);
  root.style.setProperty('--md-sys-color-on-primary', p.onPrimary);
  root.style.setProperty('--md-sys-color-primary-container', p.primaryContainer);
  root.style.setProperty('--md-sys-color-on-primary-container', p.onPrimaryContainer);

  root.style.setProperty('--md-sys-color-secondary', p.secondary);
  root.style.setProperty('--md-sys-color-on-secondary', p.onSecondary);
  root.style.setProperty('--md-sys-color-secondary-container', p.secondaryContainer);
  root.style.setProperty('--md-sys-color-on-secondary-container', p.onSecondaryContainer);

  root.style.setProperty('--md-sys-color-tertiary', p.tertiary);
  root.style.setProperty('--md-sys-color-on-tertiary', p.onTertiary);
  root.style.setProperty('--md-sys-color-tertiary-container', p.tertiaryContainer);
  root.style.setProperty('--md-sys-color-on-tertiary-container', p.onTertiaryContainer);

  root.style.setProperty('--md-sys-color-surface', p.surface);
  root.style.setProperty('--md-sys-color-surface-dim', p.surfaceDim);
  root.style.setProperty('--md-sys-color-surface-bright', p.surfaceBright);
  root.style.setProperty('--md-sys-color-surface-container-lowest', p.surfaceContainerLowest);
  root.style.setProperty('--md-sys-color-surface-container-low', p.surfaceContainerLow);
  root.style.setProperty('--md-sys-color-surface-container', p.surfaceContainer);
  root.style.setProperty('--md-sys-color-surface-container-high', p.surfaceContainerHigh);
  root.style.setProperty('--md-sys-color-surface-container-highest', p.surfaceContainerHighest);

  root.style.setProperty('--md-sys-color-on-surface', p.onSurface);
  root.style.setProperty('--md-sys-color-on-surface-variant', p.onSurfaceVariant);
  root.style.setProperty('--md-sys-color-outline', p.outline);
  root.style.setProperty('--md-sys-color-outline-variant', p.outlineVariant);

  const isDark = effectiveMode !== 'light';
  const errorHex = isDark ? '#ffb4ab' : '#ba1a1a';
  const onErrorHex = isDark ? '#690005' : '#ffffff';
  const errorContainerHex = isDark ? '#93000a' : '#ffdad6';
  const onErrorContainerHex = isDark ? '#ffdad6' : '#410002';

  root.style.setProperty('--md-sys-color-error', errorHex);
  root.style.setProperty('--md-sys-color-on-error', onErrorHex);
  root.style.setProperty('--md-sys-color-error-container', errorContainerHex);
  root.style.setProperty('--md-sys-color-on-error-container', onErrorContainerHex);

  // m3-svelte compatibility variables (full palette)
  root.style.setProperty('--m3c-primary', p.primary);
  root.style.setProperty('--m3c-on-primary', p.onPrimary);
  root.style.setProperty('--m3c-primary-container', p.primaryContainer);
  root.style.setProperty('--m3c-on-primary-container', p.onPrimaryContainer);

  root.style.setProperty('--m3c-secondary', p.secondary);
  root.style.setProperty('--m3c-on-secondary', p.onSecondary);
  root.style.setProperty('--m3c-secondary-container', p.secondaryContainer);
  root.style.setProperty('--m3c-on-secondary-container', p.onSecondaryContainer);

  root.style.setProperty('--m3c-tertiary', p.tertiary);
  root.style.setProperty('--m3c-on-tertiary', p.onTertiary);
  root.style.setProperty('--m3c-tertiary-container', p.tertiaryContainer);
  root.style.setProperty('--m3c-on-tertiary-container', p.onTertiaryContainer);

  root.style.setProperty('--m3c-surface', p.surface);
  root.style.setProperty('--m3c-surface-container', p.surfaceContainer);
  root.style.setProperty('--m3c-surface-container-high', p.surfaceContainerHigh);
  root.style.setProperty('--m3c-surface-container-highest', p.surfaceContainerHighest);
  root.style.setProperty('--m3c-surface-container-low', p.surfaceContainerLow);
  root.style.setProperty('--m3c-surface-container-lowest', p.surfaceContainerLowest);
  root.style.setProperty('--m3c-surface-dim', p.surfaceDim);
  root.style.setProperty('--m3c-surface-bright', p.surfaceBright);
  root.style.setProperty('--m3c-on-surface', p.onSurface);
  root.style.setProperty('--m3c-on-surface-variant', p.onSurfaceVariant);
  root.style.setProperty('--m3c-outline', p.outline);
  root.style.setProperty('--m3c-outline-variant', p.outlineVariant);
  root.style.setProperty('--m3c-shadow', '#000000');
  root.style.setProperty('--m3c-scrim', '#000000');
  root.style.setProperty('--m3c-on-on-primary', p.onPrimary);
  root.style.setProperty('--m3c-error', errorHex);
  root.style.setProperty('--m3c-on-error', onErrorHex);
  root.style.setProperty('--m3c-error-container', errorContainerHex);
  root.style.setProperty('--m3c-on-error-container', onErrorContainerHex);

  // Material Design 3 Shape Tokens
  root.style.setProperty('--md-sys-shape-none', '0px');
  root.style.setProperty('--md-sys-shape-extra-small', '4px');
  root.style.setProperty('--md-sys-shape-small', '8px');
  root.style.setProperty('--md-sys-shape-medium', '12px');
  root.style.setProperty('--md-sys-shape-large', '16px');
  root.style.setProperty('--md-sys-shape-extra-large', '28px');
  root.style.setProperty('--md-sys-shape-full', '9999px');

  // Compatibility aliases for --md3-shape-* and --md3-color-*
  root.style.setProperty('--md3-shape-small', '8px');
  root.style.setProperty('--md3-shape-medium', '12px');
  root.style.setProperty('--md3-shape-large', '16px');
  root.style.setProperty('--md3-shape-full', '9999px');

  root.style.setProperty('--md3-color-primary', p.primary);
  root.style.setProperty('--md3-color-on-primary', p.onPrimary);
  root.style.setProperty('--md3-color-primary-container', p.primaryContainer);
  root.style.setProperty('--md3-color-on-primary-container', p.onPrimaryContainer);
  root.style.setProperty('--md3-color-secondary', p.secondary);
  root.style.setProperty('--md3-color-on-secondary', p.onSecondary);
  root.style.setProperty('--md3-color-secondary-container', p.secondaryContainer);
  root.style.setProperty('--md3-color-on-secondary-container', p.onSecondaryContainer);
  root.style.setProperty('--md3-color-surface', p.surface);
  root.style.setProperty('--md3-color-surface-dim', p.surfaceDim);
  root.style.setProperty('--md3-color-surface-bright', p.surfaceBright);
  root.style.setProperty('--md3-color-surface-container-lowest', p.surfaceContainerLowest);
  root.style.setProperty('--md3-color-surface-container-low', p.surfaceContainerLow);
  root.style.setProperty('--md3-color-surface-container', p.surfaceContainer);
  root.style.setProperty('--md3-color-surface-container-high', p.surfaceContainerHigh);
  root.style.setProperty('--md3-color-surface-container-highest', p.surfaceContainerHighest);
  root.style.setProperty('--md3-color-on-surface', p.onSurface);
  root.style.setProperty('--md3-color-on-surface-variant', p.onSurfaceVariant);
  root.style.setProperty('--md3-color-outline', p.outline);
  root.style.setProperty('--md3-color-outline-variant', p.outlineVariant);
  root.style.setProperty('--md3-color-error', '#ffb4ab');
  root.style.setProperty('--md3-color-error-container', '#93000a');
  root.style.setProperty('--md3-color-on-error-container', '#ffdad6');

  // Window background and translucency handling
  if (translucency !== 'none') {
    const alpha = effectiveMode === 'amoled' ? 0.88 : 0.78;
    root.style.setProperty(
      '--app-window-bg',
      `rgba(${p.surfaceRgb.r}, ${p.surfaceRgb.g}, ${p.surfaceRgb.b}, ${alpha})`
    );
    root.style.setProperty('--app-backdrop-blur', 'blur(28px) saturate(180%)');
  } else {
    root.style.setProperty('--app-window-bg', p.surface);
    root.style.setProperty('--app-backdrop-blur', 'none');
  }

  // Set theme attributes on html
  root.setAttribute('data-theme', effectiveMode);
  root.setAttribute('data-theme-preference', mode);
  root.setAttribute('data-translucency', translucency);
}

// Dynamic Cover Accent Color Extractor (offscreen canvas pixel sampling)
const MAX_COVER_ACCENT_CACHE_ENTRIES = 64;
const coverAccentCache = new Map();
const pendingCoverAccents = new Map();

export async function extractCoverAccent(imageUrl) {
  if (!imageUrl) return null;
  if (coverAccentCache.has(imageUrl)) {
    const accent = coverAccentCache.get(imageUrl);
    coverAccentCache.delete(imageUrl);
    coverAccentCache.set(imageUrl, accent);
    return accent;
  }
  if (pendingCoverAccents.has(imageUrl)) return pendingCoverAccents.get(imageUrl);

  const pending = new Promise((resolve) => {
    const img = new Image();
    img.crossOrigin = 'Anonymous';
    img.decoding = 'async';
    img.onload = () => {
      let canvas;
      let accent = null;
      try {
        canvas = document.createElement('canvas');
        const ctx = canvas.getContext('2d');
        canvas.width = 32;
        canvas.height = 32;
        ctx.drawImage(img, 0, 0, 32, 32);
        const data = ctx.getImageData(0, 0, 32, 32).data;

        let bestHex = null;
        let maxSaturation = 0.15;

        for (let i = 0; i < data.length; i += 16) {
          const r = data[i];
          const g = data[i + 1];
          const b = data[i + 2];
          const a = data[i + 3];
          if (a < 128) continue;

          const hsl = rgbToHsl(r, g, b);
          // Prefer colorful tones over pure black/white
          if (hsl.l > 0.15 && hsl.l < 0.85 && hsl.s > maxSaturation) {
            maxSaturation = hsl.s;
            bestHex = rgbToHex(r, g, b);
          }
        }
        accent = bestHex;
      } catch {
        accent = null;
      } finally {
        if (canvas) {
          canvas.width = 0;
          canvas.height = 0;
        }
        img.onload = null;
        img.onerror = null;
      }
      resolve(accent);
    };
    img.onerror = () => {
      img.onload = null;
      img.onerror = null;
      resolve(null);
    };
    img.src = imageUrl;
  });

  pendingCoverAccents.set(imageUrl, pending);
  try {
    const accent = await pending;
    if (accent) {
      coverAccentCache.set(imageUrl, accent);
      if (coverAccentCache.size > MAX_COVER_ACCENT_CACHE_ENTRIES) {
        coverAccentCache.delete(coverAccentCache.keys().next().value);
      }
    }
    return accent;
  } finally {
    pendingCoverAccents.delete(imageUrl);
  }
}
