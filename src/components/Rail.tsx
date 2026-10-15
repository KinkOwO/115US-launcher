import type { ComponentType } from "react";
import { DevIcon, FeedbackIcon, HomeIcon, ModsIcon, SavesIcon, SettingsIcon, ToolsIcon } from "./icons";

export type PageKey = "home" | "mods" | "tools" | "saves" | "settings" | "feedback" | "dev";

type Page = { key: PageKey; label: string; Icon: ComponentType<{ size?: number }> };

/// 除「主页」外都只是占位页，不挂任何原有功能。
export const PAGES: Page[] = [
  { key: "home", label: "主页", Icon: HomeIcon },
  { key: "mods", label: "mod", Icon: ModsIcon },
  { key: "tools", label: "工具", Icon: ToolsIcon },
  { key: "saves", label: "存档", Icon: SavesIcon },
  { key: "settings", label: "设置", Icon: SettingsIcon },
  { key: "feedback", label: "反馈", Icon: FeedbackIcon },
  { key: "dev", label: "开发者", Icon: DevIcon },
];

export default function Rail({ page, onSelect }: { page: PageKey; onSelect: (key: PageKey) => void }) {
  const item = ({ key, label, Icon }: Page) => (
    <button
      key={key}
      className="rail-item"
      data-tip={label}
      aria-label={label}
      aria-current={page === key ? "page" : undefined}
      onClick={() => onSelect(key)}
    >
      <Icon size={21} />
    </button>
  );

  return (
    <nav className="rail">
      <div className="rail-nav">{PAGES.slice(0, 6).map(item)}</div>
      <div className="rail-foot">{PAGES.slice(6).map(item)}</div>
    </nav>
  );
}
