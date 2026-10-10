import { describe, it, expect, beforeEach, vi } from 'vitest';
import {
  hexToRgb,
  rgbToHex,
  rgbToHsl,
  hslToRgb,
  resolveThemeMode,
  onSystemThemeChange,
  generateMaterialPalette,
  applyTheme,
  extractCoverAccent,
  PRESET_SEEDS,
} from './theme.js';

describe('Color Utilities', () => {
  it('converts 6-digit hex to RGB correctly', () => {
    expect(hexToRgb('#10B981')).toEqual({ r: 16, g: 185, b: 129 });
    expect(hexToRgb('#000000')).toEqual({ r: 0, g: 0, b: 0 });
    expect(hexToRgb('#ffffff')).toEqual({ r: 255, g: 255, b: 255 });
  });

  it('converts 3-digit shorthand hex to RGB', () => {
    expect(hexToRgb('#fff')).toEqual({ r: 255, g: 255, b: 255 });
    expect(hexToRgb('#000')).toEqual({ r: 0, g: 0, b: 0 });
    expect(hexToRgb('#f00')).toEqual({ r: 255, g: 0, b: 0 });
  });

  it('converts RGB to hex string with clamping', () => {
    expect(rgbToHex(16, 185, 129)).toBe('#10b981');
    expect(rgbToHex(0, 0, 0)).toBe('#000000');
    expect(rgbToHex(255, 255, 255)).toBe('#ffffff');
    expect(rgbToHex(300, -10, 50)).toBe('#ff0032');
  });

  it('round-trips RGB through HSL accurately', () => {
    const original = { r: 120, g: 60, b: 200 };
    const hsl = rgbToHsl(original.r, original.g, original.b);
    const roundtrip = hslToRgb(hsl.h, hsl.s, hsl.l);

    expect(Math.abs(roundtrip.r - original.r)).toBeLessThanOrEqual(1);
    expect(Math.abs(roundtrip.g - original.g)).toBeLessThanOrEqual(1);
    expect(Math.abs(roundtrip.b - original.b)).toBeLessThanOrEqual(1);
  });
});

describe('Material You Palette Generation', () => {
  const hexRegex = /^#[0-9a-fA-F]{6}$/;

  it('includes all preset seeds', () => {
    expect(PRESET_SEEDS.length).toBeGreaterThanOrEqual(7);
    expect(PRESET_SEEDS[0].name).toBe('Emerald');
  });

  it('generates complete dark mode palette with valid hex tokens', () => {
    const palette = generateMaterialPalette('#10B981', 'dark');

    expect(palette.primary).toMatch(hexRegex);
    expect(palette.onPrimary).toMatch(hexRegex);
    expect(palette.primaryContainer).toMatch(hexRegex);
    expect(palette.secondary).toMatch(hexRegex);
    expect(palette.surface).toMatch(hexRegex);
    expect(palette.surfaceContainer).toMatch(hexRegex);
    expect(palette.onSurface).toMatch(hexRegex);
    expect(palette.outline).toMatch(hexRegex);

    expect(palette.primaryRgb).toBeDefined();
    expect(palette.surfaceRgb).toBeDefined();
  });

  it('generates light mode palette with lighter surfaces', () => {
    const darkPalette = generateMaterialPalette('#2563EB', 'dark');
    const lightPalette = generateMaterialPalette('#2563EB', 'light');

    expect(lightPalette.primary).toMatch(hexRegex);
    expect(lightPalette.surface).toMatch(hexRegex);
    // Light mode surface should have higher RGB luminance than dark mode surface
    const darkLum = darkPalette.surfaceRgb.r + darkPalette.surfaceRgb.g + darkPalette.surfaceRgb.b;
    const lightLum = lightPalette.surfaceRgb.r + lightPalette.surfaceRgb.g + lightPalette.surfaceRgb.b;
    expect(lightLum).toBeGreaterThan(darkLum);
  });

  it('generates AMOLED mode palette with pitch-black surfaces', () => {
    const amoled = generateMaterialPalette('#7C3AED', 'amoled');

    expect(amoled.surface).toBe('#000000');
    expect(amoled.surfaceDim).toBe('#000000');
    expect(amoled.surfaceContainerLowest).toBe('#000000');
    expect(amoled.surfaceRgb).toEqual({ r: 0, g: 0, b: 0 });
  });

  it('resolves system mode based on matchMedia preferences', () => {
    expect(resolveThemeMode('dark')).toBe('dark');
    expect(resolveThemeMode('light')).toBe('light');
    expect(resolveThemeMode('amoled')).toBe('amoled');

    // Without window or matchMedia, defaults to dark
    expect(resolveThemeMode('system')).toBe('dark');

    // Mock matchMedia for light mode
    global.window = {
      matchMedia: vi.fn().mockImplementation((query) => ({
        matches: false,
        media: query,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
      })),
    };
    expect(resolveThemeMode('system')).toBe('light');

    // Mock matchMedia for dark mode
    global.window.matchMedia = vi.fn().mockImplementation((query) => ({
      matches: true,
      media: query,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    }));
    expect(resolveThemeMode('system')).toBe('dark');
  });

  it('generates system mode palette seamlessly', () => {
    const palette = generateMaterialPalette('#10B981', 'system');
    expect(palette.primary).toMatch(hexRegex);
    expect(palette.surface).toMatch(hexRegex);
  });
});

describe('DOM Theme Application', () => {
  beforeEach(() => {
    // Setup document mock for Node/jsdom
    if (typeof document === 'undefined') {
      const styles = {};
      const attrs = {};
      global.document = {
        documentElement: {
          style: {
            setProperty: (k, v) => {
              styles[k] = v;
            },
            getPropertyValue: (k) => styles[k],
          },
          setAttribute: (k, v) => {
            attrs[k] = v;
          },
          getAttribute: (k) => attrs[k],
        },
      };
    }
  });

  it('sets CSS variables and theme attributes on root element', () => {
    applyTheme('#10B981', 'dark', 'mica');
    const root = document.documentElement;

    expect(root.getAttribute('data-theme')).toBe('dark');
    expect(root.getAttribute('data-translucency')).toBe('mica');
    expect(root.style.getPropertyValue('--md-sys-color-primary'))
      .toBe(generateMaterialPalette('#10B981', 'dark').primary);
    expect(root.style.getPropertyValue('--m3c-primary')).toBeDefined();
    expect(root.style.getPropertyValue('--app-window-bg')).toContain('rgba');
  });

  it('extractCoverAccent returns null on empty image URL', async () => {
    const accent = await extractCoverAccent(null);
    expect(accent).toBeNull();
    const emptyAccent = await extractCoverAccent('');
    expect(emptyAccent).toBeNull();
  });

  it('shares and caches cover accent extraction for repeated image URLs', async () => {
    const previousImage = globalThis.Image;
    const previousCreateElement = document.createElement;
    let imageCount = 0;
    globalThis.Image = class MockImage {
      constructor() {
        imageCount += 1;
      }

      set src(value) {
        this.url = value;
        queueMicrotask(() => this.onload?.());
      }
    };
    document.createElement = () => ({
      width: 0,
      height: 0,
      getContext: () => ({
        drawImage: vi.fn(),
        getImageData: () => {
          const data = new Uint8ClampedArray(32 * 32 * 4);
          for (let index = 0; index < data.length; index += 4) {
            data.set([200, 50, 50, 255], index);
          }
          return { data };
        },
      }),
    });

    try {
      const [first, concurrent] = await Promise.all([
        extractCoverAccent('https://example.test/cover.jpg'),
        extractCoverAccent('https://example.test/cover.jpg'),
      ]);
      const cached = await extractCoverAccent('https://example.test/cover.jpg');

      expect(first).toMatch(/^#[0-9a-f]{6}$/);
      expect(concurrent).toBe(first);
      expect(cached).toBe(first);
      expect(imageCount).toBe(1);
    } finally {
      globalThis.Image = previousImage;
      document.createElement = previousCreateElement;
    }
  });
});
