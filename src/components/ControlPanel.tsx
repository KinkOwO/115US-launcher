import { useEffect, useState } from "react";
import type { ConfigView, Readiness } from "../bridge";

export default function ControlPanel({
  config,
  readiness,
  busy,
  onAccount,
  onPickClient,
  onDiscoverClient,
}: {
  config: ConfigView | null;
  readiness: Readiness | null;
  busy: boolean;
  onAccount: (account: string) => Promise<void>;
  onPickClient: () => Promise<void>;
  onDiscoverClient: () => Promise<void>;
}) {
  const [account, setAccount] = useState("");

  useEffect(() => {
    if (config) setAccount(config.account);
  }, [config]);

  const dirty = config !== null && account.trim() !== config.account;

  return (
    <section className="card">
      <h2>本次会话</h2>

      <div className="field">
        <label htmlFor="account">登录账号（下次「开始游戏」生效）</label>
        <div className="row">
          <input
            id="account"
            type="text"
            value={account}
            spellCheck={false}
            onChange={(event) => setAccount(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && dirty && !busy) void onAccount(account);
            }}
          />
          <button className="btn" data-tone="accent" disabled={busy || !dirty} onClick={() => onAccount(account)}>
            应用
          </button>
        </div>
        {config && config.accounts.length > 1 ? (
          <div className="row">
            {config.accounts
              .filter((name) => name !== account)
              .slice(0, 6)
              .map((name) => (
                <button className="btn" key={name} disabled={busy} onClick={() => setAccount(name)}>
                  {name}
                </button>
              ))}
          </div>
        ) : null}
        <p className="hint">字母、数字、下划线与短横线，最长 32；默认 probe。</p>
      </div>

      <div className="field">
        <label>游戏客户端目录</label>
        <div className="path">{readiness?.client_dir ?? "…"}</div>
        <div className="row">
          <button className="btn" disabled={busy} onClick={onDiscoverClient}>
            自动识别
          </button>
          <button className="btn" disabled={busy} onClick={onPickClient}>
            手动选择…
          </button>
        </div>
      </div>

      <div className="field">
        <label>存档</label>
        <div className="path">{config?.storage_detail ?? "…"}</div>
        <p className="hint">启动器只在缺配置时补一份 SQLite 档，绝不覆盖或预建库文件。</p>
      </div>
    </section>
  );
}
