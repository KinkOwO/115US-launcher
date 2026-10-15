import { useCallback, useEffect, useState } from "react";
import { commands, errText, type ModsState, type Phase, type ServerMod } from "../bridge";

function humanSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/** 这一代服务端只在启动时读一次名单，所以只有停下来时才允许改。 */
function locked(phase: Phase): boolean {
  return phase === "starting" || phase === "running" || phase === "stopping";
}

export default function ModsPage({
  phase,
  onNotice,
}: {
  phase: Phase;
  onNotice: (text: string, tone?: "ok" | "err") => void;
}) {
  const [state, setState] = useState<ModsState | null>(null);
  const [busy, setBusy] = useState(false);

  const reload = useCallback(async () => {
    try {
      setState(await commands.modsState());
    } catch (err) {
      onNotice(errText(err), "err");
    }
  }, [onNotice]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const toggle = async (mod: ServerMod) => {
    setBusy(true);
    try {
      const record = await commands.setServerMod(mod.id, !mod.enabled);
      onNotice(record.message);
      await reload();
    } catch (err) {
      onNotice(errText(err), "err");
    } finally {
      setBusy(false);
    }
  };

  const server = state?.server;
  const scripts = state?.scripts;
  const client = state?.client;
  const modkit = state?.modkit;
  const frozen = locked(phase) || busy;
  const loaded = server?.mods.filter((mod) => mod.seen).length ?? 0;

  return (
    <div className="body">
      <div className="column">
        <section className="card">
          <h2>服务端 mod</h2>
          <div className="row">
            <span className="chip" data-tone={loaded > 0 ? "ok" : undefined}>
              <i />
              本场装载 {loaded} 个
            </span>
            <span className="chip">
              {server?.enabled_present ? "已有停用名单" : "没有停用名单"}
            </span>
            <span className="spacer" />
            <button className="btn" data-tone="ghost" disabled={busy} onClick={() => void reload()}>
              重新扫描
            </button>
          </div>

          {server?.broken_note ? <p className="hint" data-tone="err">{server.broken_note}</p> : null}
          {server?.ignored_note ? <p className="hint" data-tone="err">{server.ignored_note}</p> : null}

          {server && server.mods.length > 0 ? (
            server.mods.map((mod) => (
              <div className="field" key={mod.id}>
                <button
                  className="switch"
                  data-on={mod.enabled ? "true" : "false"}
                  disabled={frozen}
                  onClick={() => void toggle(mod)}
                >
                  <span>
                    {mod.id}
                    <br />
                    <em className="hint">{mod.purpose || "服务端没报这一条的用途"}</em>
                  </span>
                  <span className="switch-track" />
                </button>
                <div className="row">
                  <span className="chip" data-tone={mod.seen ? "ok" : undefined}>
                    <i />
                    {mod.seen
                      ? mod.enabled
                        ? "本场确实装载了它"
                        : "本场装载了，下次启动就不再装载"
                      : mod.enabled
                        ? "本场没装载（还没跑过）"
                        : "已停用，本场未装载"}
                  </span>
                </div>
              </div>
            ))
          ) : (
            <p className="hint">
              这一场服务端没注册任何服务端 mod。先跑一次游戏再回到本页，网关报的清单会出现在这里。
            </p>
          )}

          <p className="hint">
            {frozen
              ? "服务端正开着。它只在启动时读一次这份名单，所以停服之后这里的开关才让按。"
              : server?.gateway_note
                ? `上一场日志说的：${server.gateway_note}（改完开关要重新「开始游戏」才生效）`
                : "服务端启动时读一次这份名单：改完开关要重新「开始游戏」才生效。"}
          </p>
          <p className="path">{server ? `${server.mods_dir}\\enabled.json` : "…"}</p>
        </section>

        <section className="card">
          <h2>奖励规则脚本</h2>
          <div className="row">
            <span className="chip" data-tone={(scripts?.scripts.length ?? 0) > 0 ? "ok" : undefined}>
              <i />
              盘上 {scripts?.scripts.length ?? 0} 份
            </span>
            <span className="chip" data-tone={(scripts?.provided.length ?? 0) > 0 ? "ok" : undefined}>
              <i />
              mod 自带 {scripts?.provided.length ?? 0} 份
            </span>
          </div>

          {scripts && scripts.scripts.length + scripts.provided.length > 0 ? (
            <ul className="scope">
              {scripts.provided.map((item) => (
                <li key={item}>{item}（由 mod 提供，不落盘）</li>
              ))}
              {scripts.scripts.map((file) => (
                <li key={file.name}>
                  {file.name} · {humanSize(file.size)}
                </li>
              ))}
            </ul>
          ) : null}

          <p className="hint">
            启动器不往这个目录写东西。要加减脚本就把 .lua 放进去，下次「开始游戏」时服务端自己读。
          </p>
          <p className="path">{scripts?.dir ?? "…"}</p>
        </section>
      </div>

      <div className="column">
        <section className="card">
          <h2>客户端插件</h2>
          <div className="row">
            <span className="chip" data-tone={client?.present ? undefined : "err"}>
              <i />
              {client?.present ? "插件目录在场" : "插件目录不在"}
            </span>
            <span className="chip">
              {client?.auto_discover ? "清单外的 DLL 也会被加载" : "只加载清单里列的"}
            </span>
            <span className="chip">{client?.plugins.length ?? 0} 个</span>
          </div>

          {client && client.plugins.length > 0 ? (
            <div className="field">
              {client.plugins.map((plugin) => (
                <div className="health-row" data-level={plugin.enabled ? "ok" : "warn"} key={plugin.dll}>
                  <span className="mark">{plugin.enabled ? "✓" : "—"}</span>
                  <div>
                    <div className="health-name">
                      {plugin.id}
                      {plugin.in_manifest ? "" : "（清单外，靠自动发现）"}
                    </div>
                    <div className="health-value">
                      {plugin.dll}
                      {plugin.ready === true ? " · 自检就绪" : ""}
                      {plugin.ready === false ? ` · 自检未过：${plugin.reject_reason || "原因未写"}` : ""}
                      {plugin.deps.length > 0 ? ` · 先加载 ${plugin.deps.join("、")}` : ""}
                    </div>
                  </div>
                </div>
              ))}
            </div>
          ) : (
            <p className="hint">没读到插件清单：客户端这一侧目前没有 mod 插件。</p>
          )}

          {client && client.host_tail.length > 0 ? (
            <div className="field">
              <label>客户端宿主日志尾部</label>
              <ul className="scope">
                {client.host_tail.map((line) => (
                  <li key={line}>{line}</li>
                ))}
              </ul>
            </div>
          ) : null}

          <p className="hint">
            这一侧启动器只看不动：开或关要写客户端目录里的清单，那是改游戏本体，不在启动器的权限里。
          </p>
          <p className="path">{client?.dir ?? "…"}</p>
        </section>

        <section className="card">
          <h2>mod 安装层</h2>
          <div className="row">
            <span className="chip" data-tone={modkit?.present ? "ok" : undefined}>
              <i />
              {modkit?.present ? "有落位登记表" : "没有登记表"}
            </span>
            <span className="chip">{modkit?.total ?? 0} 个 mod</span>
            <span className="chip">{modkit?.entries ?? 0} 处落位</span>
          </div>

          {modkit && modkit.risky.length > 0 ? (
            <div className="field">
              <label>动到客户端本体的</label>
              <ul className="scope" data-tone="never">
                {modkit.risky.map((risk) => (
                  <li key={`${risk.id}-${risk.target}`}>
                    {risk.id} → {risk.target}（{risk.kind}）
                  </li>
                ))}
              </ul>
              <p className="hint" data-tone="err">
                这些件动的是客户端本体。回滚必须成对：数据文件与它的密钥文件是一对，
                汉化代理 DLL 与插件清单是一对；只还原其中半件，客户端可能直接起不来。
                要撤就用 mod 安装工具整对撤，启动器这里只报现状、不放回滚按钮。
              </p>
            </div>
          ) : null}

          <p className="path">{modkit?.registry ?? "…"}</p>
        </section>
      </div>
    </div>
  );
}
