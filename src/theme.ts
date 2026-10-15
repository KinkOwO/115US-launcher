import { useEffect, useState } from "react";

export type Theme = "dark" | "light";

const KEY = "launcher.theme";

function read(): Theme {
  return window.localStorage.getItem(KEY) === "light" ? "light" : "dark";
}

/// 主题存在 localStorage：主窗与浮出的面板窗口各自订阅，切主题不用跨窗口传话。
export function useTheme() {
  const [theme, setTheme] = useState<Theme>(read);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    window.localStorage.setItem(KEY, theme);
  }, [theme]);

  useEffect(() => {
    const sync = (event: StorageEvent) => {
      if (event.key === KEY) setTheme(event.newValue === "light" ? "light" : "dark");
    };
    window.addEventListener("storage", sync);
    return () => window.removeEventListener("storage", sync);
  }, []);

  const toggleTheme = () => setTheme((prev) => (prev === "dark" ? "light" : "dark"));
  return { theme, toggleTheme };
}
