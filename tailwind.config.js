/** @type {import('tailwindcss').Config} */
// Wazuh-threat-hunter palette (DESIGN.md §8.2): near-black warm-neutral base, subtle-bordered
// cards, a single AMBER accent for interactive/active state, green/red used sparingly.
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        base: {
          900: "#0a0a0b", // app background (deepest)
          800: "#0d0d0f", // terminal / data surface
          700: "#141416", // panels
          600: "#1c1c20", // raised / hover
          500: "#2a2a2e", // borders
          400: "#34343a", // border emphasis
        },
        ink: {
          DEFAULT: "#e9e9ec",
          dim: "#9b9ba4",
          faint: "#5e5e67",
        },
        accent: {
          DEFAULT: "#e8833a", // amber — the signature wazuh accent
          soft: "#c96f2f",
          glow: "#f0954f",
        },
        ok: "#3fb950",
        warn: "#d6a02a",
        danger: "#f85149",
        mono: "#8fd6a8", // terminal command text
      },
      fontFamily: {
        sans: ['"Segoe UI Variable"', '"Segoe UI"', "system-ui", "sans-serif"],
        mono: ['"Cascadia Code"', '"JetBrains Mono"', "Consolas", "monospace"],
      },
      borderRadius: {
        card: "10px",
      },
    },
  },
  plugins: [],
};
