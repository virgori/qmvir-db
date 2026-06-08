/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  theme: {
    extend: {
      colors: {
        "qm-bg": "#0f1117",
        "qm-surface": "#1a1d27",
        "qm-border": "#2a2d3a",
        "qm-accent": "#3b82f6",
        "qm-text": "#e2e8f0",
        "qm-muted": "#64748b",
        "qm-success": "#10b981",
        "qm-warn": "#f59e0b",
        "qm-error": "#ef4444",
      },
    },
  },
  plugins: [],
};
