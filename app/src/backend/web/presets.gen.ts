// GENERATED from app/src-tauri/src/settings.rs list_presets() (Phase 119,
// WEB-DESIGN F5) — the desktop's theme presets for the browser's
// settings_get_presets / settings_apply_preset. Do not edit by hand:
// regenerate when the Rust table changes (vault frontend-lib).

import type { Theme } from "../../bindings/Theme";

export interface PresetEntry {
  id: string;
  label: string;
  theme: Theme;
}

export const PRESETS: PresetEntry[] = [
  {
    "id": "tokyo-night",
    "label": "Tokyo Night",
    "theme": {
      "preset": "tokyo-night",
      "accent": "#7aa2f7",
      "background": "#0e1116",
      "surface": "#161b22",
      "border": "#21262d",
      "text_primary": "#e6edf3",
      "text_secondary": "#7d8590",
      "success": "#4ec9b0",
      "warning": "#e0af68",
      "error": "#f7768e",
      "ansi": {
        "black": "#15161e",
        "red": "#f7768e",
        "green": "#9ece6a",
        "yellow": "#e0af68",
        "blue": "#7aa2f7",
        "magenta": "#bb9af7",
        "cyan": "#7dcfff",
        "white": "#a9b1d6",
        "bright_black": "#414868",
        "bright_red": "#ff7a93",
        "bright_green": "#b9f27c",
        "bright_yellow": "#ff9e64",
        "bright_blue": "#7da6ff",
        "bright_magenta": "#bb9af7",
        "bright_cyan": "#0db9d7",
        "bright_white": "#c0caf5"
      }
    }
  },
  {
    "id": "dracula",
    "label": "Dracula",
    "theme": {
      "preset": "dracula",
      "accent": "#bd93f9",
      "background": "#282a36",
      "surface": "#21222c",
      "border": "#44475a",
      "text_primary": "#f8f8f2",
      "text_secondary": "#6272a4",
      "success": "#50fa7b",
      "warning": "#f1fa8c",
      "error": "#ff5555",
      "ansi": {
        "black": "#21222c",
        "red": "#ff5555",
        "green": "#50fa7b",
        "yellow": "#f1fa8c",
        "blue": "#bd93f9",
        "magenta": "#ff79c6",
        "cyan": "#8be9fd",
        "white": "#f8f8f2",
        "bright_black": "#6272a4",
        "bright_red": "#ff6e6e",
        "bright_green": "#69ff94",
        "bright_yellow": "#ffffa5",
        "bright_blue": "#d6acff",
        "bright_magenta": "#ff92df",
        "bright_cyan": "#a4ffff",
        "bright_white": "#ffffff"
      }
    }
  },
  {
    "id": "solarized-dark",
    "label": "Solarized Dark",
    "theme": {
      "preset": "solarized-dark",
      "accent": "#268bd2",
      "background": "#002b36",
      "surface": "#073642",
      "border": "#586e75",
      "text_primary": "#eee8d5",
      "text_secondary": "#93a1a1",
      "success": "#859900",
      "warning": "#b58900",
      "error": "#dc322f",
      "ansi": {
        "black": "#073642",
        "red": "#dc322f",
        "green": "#859900",
        "yellow": "#b58900",
        "blue": "#268bd2",
        "magenta": "#d33682",
        "cyan": "#2aa198",
        "white": "#eee8d5",
        "bright_black": "#002b36",
        "bright_red": "#cb4b16",
        "bright_green": "#586e75",
        "bright_yellow": "#657b83",
        "bright_blue": "#839496",
        "bright_magenta": "#6c71c4",
        "bright_cyan": "#93a1a1",
        "bright_white": "#fdf6e3"
      }
    }
  },
  {
    "id": "nord",
    "label": "Nord",
    "theme": {
      "preset": "nord",
      "accent": "#88c0d0",
      "background": "#2e3440",
      "surface": "#3b4252",
      "border": "#4c566a",
      "text_primary": "#eceff4",
      "text_secondary": "#d8dee9",
      "success": "#a3be8c",
      "warning": "#ebcb8b",
      "error": "#bf616a",
      "ansi": {
        "black": "#3b4252",
        "red": "#bf616a",
        "green": "#a3be8c",
        "yellow": "#ebcb8b",
        "blue": "#81a1c1",
        "magenta": "#b48ead",
        "cyan": "#88c0d0",
        "white": "#e5e9f0",
        "bright_black": "#4c566a",
        "bright_red": "#bf616a",
        "bright_green": "#a3be8c",
        "bright_yellow": "#ebcb8b",
        "bright_blue": "#81a1c1",
        "bright_magenta": "#b48ead",
        "bright_cyan": "#8fbcbb",
        "bright_white": "#eceff4"
      }
    }
  },
  {
    "id": "solarized-light",
    "label": "Solarized Light",
    "theme": {
      "preset": "solarized-light",
      "accent": "#268bd2",
      "background": "#fdf6e3",
      "surface": "#eee8d5",
      "border": "#93a1a1",
      "text_primary": "#073642",
      "text_secondary": "#586e75",
      "success": "#859900",
      "warning": "#b58900",
      "error": "#dc322f",
      "ansi": {
        "black": "#073642",
        "red": "#dc322f",
        "green": "#859900",
        "yellow": "#b58900",
        "blue": "#268bd2",
        "magenta": "#d33682",
        "cyan": "#2aa198",
        "white": "#eee8d5",
        "bright_black": "#002b36",
        "bright_red": "#cb4b16",
        "bright_green": "#586e75",
        "bright_yellow": "#657b83",
        "bright_blue": "#839496",
        "bright_magenta": "#6c71c4",
        "bright_cyan": "#93a1a1",
        "bright_white": "#fdf6e3"
      }
    }
  },
  {
    "id": "industry",
    "label": "Industry",
    "theme": {
      "preset": "industry",
      "accent": "#5980a6",
      "background": "#f2f2f3",
      "surface": "#f5f5f8",
      "border": "#d4d4d7",
      "text_primary": "#1d1f20",
      "text_secondary": "#5d5d60",
      "success": "#3d7a54",
      "warning": "#9a6a00",
      "error": "#b3392f",
      "ansi": {
        "black": "#24292f",
        "red": "#cf222e",
        "green": "#116329",
        "yellow": "#9a6700",
        "blue": "#3d6a94",
        "magenta": "#8250df",
        "cyan": "#1b7c83",
        "white": "#57606a",
        "bright_black": "#656d76",
        "bright_red": "#a40e26",
        "bright_green": "#1a7f37",
        "bright_yellow": "#bf8700",
        "bright_blue": "#4d7fae",
        "bright_magenta": "#a475f9",
        "bright_cyan": "#3192aa",
        "bright_white": "#8c959f"
      }
    }
  },
  {
    "id": "broadsheet",
    "label": "Broadsheet",
    "theme": {
      "preset": "broadsheet",
      "accent": "#0088b0",
      "background": "#f3f2f2",
      "surface": "#f8f4f4",
      "border": "#d7d3d3",
      "text_primary": "#201e1d",
      "text_secondary": "#605d5d",
      "success": "#2f7d4f",
      "warning": "#9a6a00",
      "error": "#c02d3c",
      "ansi": {
        "black": "#24292f",
        "red": "#cf222e",
        "green": "#116329",
        "yellow": "#9a6700",
        "blue": "#0969da",
        "magenta": "#d6006c",
        "cyan": "#0e7a9b",
        "white": "#57606a",
        "bright_black": "#656d76",
        "bright_red": "#a40e26",
        "bright_green": "#1a7f37",
        "bright_yellow": "#bf8700",
        "bright_blue": "#218bff",
        "bright_magenta": "#aa0b56",
        "bright_cyan": "#006786",
        "bright_white": "#8c959f"
      }
    }
  },
  {
    "id": "modernist",
    "label": "Modernist",
    "theme": {
      "preset": "modernist",
      "accent": "#ec3013",
      "background": "#f3f2f2",
      "surface": "#f8f4f4",
      "border": "#201e1d",
      "text_primary": "#201e1d",
      "text_secondary": "#605d5d",
      "success": "#2f7d4f",
      "warning": "#9a6a00",
      "error": "#ec3013",
      "ansi": {
        "black": "#24292f",
        "red": "#c22a10",
        "green": "#116329",
        "yellow": "#9a6700",
        "blue": "#0969da",
        "magenta": "#8250df",
        "cyan": "#1b7c83",
        "white": "#57606a",
        "bright_black": "#656d76",
        "bright_red": "#a3230d",
        "bright_green": "#1a7f37",
        "bright_yellow": "#bf8700",
        "bright_blue": "#218bff",
        "bright_magenta": "#a475f9",
        "bright_cyan": "#3192aa",
        "bright_white": "#8c959f"
      }
    }
  },
  {
    "id": "classical",
    "label": "Classical",
    "theme": {
      "preset": "classical",
      "accent": "#b68235",
      "background": "#f3f2f2",
      "surface": "#f8f4f4",
      "border": "#d7d3d3",
      "text_primary": "#201f1d",
      "text_secondary": "#605d5d",
      "success": "#4a7a4a",
      "warning": "#b68235",
      "error": "#a3392f",
      "ansi": {
        "black": "#24292f",
        "red": "#cf222e",
        "green": "#116329",
        "yellow": "#8a6215",
        "blue": "#0969da",
        "magenta": "#8250df",
        "cyan": "#1b7c83",
        "white": "#57606a",
        "bright_black": "#656d76",
        "bright_red": "#a40e26",
        "bright_green": "#1a7f37",
        "bright_yellow": "#7d5411",
        "bright_blue": "#218bff",
        "bright_magenta": "#a475f9",
        "bright_cyan": "#3192aa",
        "bright_white": "#8c959f"
      }
    }
  },
  {
    "id": "industry-dark",
    "label": "Industry Dark",
    "theme": {
      "preset": "industry-dark",
      "accent": "#6f9fce",
      "background": "#12151a",
      "surface": "#1a1e25",
      "border": "#2b3742",
      "text_primary": "#dfe6ee",
      "text_secondary": "#8b97a4",
      "success": "#4ec9b0",
      "warning": "#e0af68",
      "error": "#f7768e",
      "ansi": {
        "black": "#1c232b",
        "red": "#e0716b",
        "green": "#74b585",
        "yellow": "#d9b26a",
        "blue": "#6f9fce",
        "magenta": "#9a8fd0",
        "cyan": "#6fc3ce",
        "white": "#c3ccd6",
        "bright_black": "#3d4a58",
        "bright_red": "#f28b85",
        "bright_green": "#8fd0a0",
        "bright_yellow": "#ecc985",
        "bright_blue": "#8fb8e0",
        "bright_magenta": "#b3a8e6",
        "bright_cyan": "#8fd9e3",
        "bright_white": "#e2e9f0"
      }
    }
  },
  {
    "id": "broadsheet-dark",
    "label": "Broadsheet Dark",
    "theme": {
      "preset": "broadsheet-dark",
      "accent": "#35b6df",
      "background": "#17161a",
      "surface": "#201e24",
      "border": "#37333b",
      "text_primary": "#ece7e4",
      "text_secondary": "#a49d9d",
      "success": "#4ec9b0",
      "warning": "#e0af68",
      "error": "#ff6b8a",
      "ansi": {
        "black": "#221f26",
        "red": "#ff6b8a",
        "green": "#7cb98a",
        "yellow": "#d4b370",
        "blue": "#6ea3c9",
        "magenta": "#ff2f86",
        "cyan": "#35b6df",
        "white": "#cfc8c6",
        "bright_black": "#494349",
        "bright_red": "#ff8ba3",
        "bright_green": "#97d0a4",
        "bright_yellow": "#e6c98d",
        "bright_blue": "#8ebcdd",
        "bright_magenta": "#ff5ea1",
        "bright_cyan": "#5ecbf0",
        "bright_white": "#ece7e4"
      }
    }
  },
  {
    "id": "modernist-dark",
    "label": "Modernist Dark",
    "theme": {
      "preset": "modernist-dark",
      "accent": "#ff563c",
      "background": "#141312",
      "surface": "#1d1b1a",
      "border": "#e7e3e1",
      "text_primary": "#f3f2f2",
      "text_secondary": "#a49d9d",
      "success": "#4ec9b0",
      "warning": "#e0af68",
      "error": "#ff563c",
      "ansi": {
        "black": "#232120",
        "red": "#ff563c",
        "green": "#86a98c",
        "yellow": "#d6c08a",
        "blue": "#8fa3b8",
        "magenta": "#c39ab0",
        "cyan": "#93b8ba",
        "white": "#d9d6d4",
        "bright_black": "#4a4644",
        "bright_red": "#ff7a5f",
        "bright_green": "#a0c2a6",
        "bright_yellow": "#e8d4a2",
        "bright_blue": "#aabccd",
        "bright_magenta": "#d7b2c6",
        "bright_cyan": "#adcfd1",
        "bright_white": "#f3f2f2"
      }
    }
  },
  {
    "id": "classical-dark",
    "label": "Classical Dark",
    "theme": {
      "preset": "classical-dark",
      "accent": "#c99a4a",
      "background": "#171512",
      "surface": "#201d18",
      "border": "#38322a",
      "text_primary": "#efe9df",
      "text_secondary": "#a99f90",
      "success": "#7fae6a",
      "warning": "#d8b06a",
      "error": "#d9736a",
      "ansi": {
        "black": "#262118",
        "red": "#d9736a",
        "green": "#94ab6e",
        "yellow": "#cd9a45",
        "blue": "#7f9cc0",
        "magenta": "#b58ab8",
        "cyan": "#7fb3a8",
        "white": "#d8cfc0",
        "bright_black": "#4d4436",
        "bright_red": "#e88f86",
        "bright_green": "#aec283",
        "bright_yellow": "#e6b562",
        "bright_blue": "#9cb5d6",
        "bright_magenta": "#cca4cf",
        "bright_cyan": "#99c9be",
        "bright_white": "#efe9df"
      }
    }
  }
];
