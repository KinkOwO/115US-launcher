import { useCallback, useEffect, useState } from "react";
import { commands, errText, type BackupItem, type SavesState } from "../bridge";

/// 服务端开着连接时整库换文件等于对着活体动底层，前后端都按这个口径拦。
function locked(phase: string): boolean {
  return phase === "starting" || phase === "running" || phase === "stopping";
}

export default function SavesPage({
  phase,
  onNotice,
}: {
  phase: string;
  onNotice: (text: string, tone?: "ok" | "err") => void;
}) {
  const [state, setState] = useState<SavesState | null>(null);
  const [picked, setPicked] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);

  const reload = useCallback(async () => {
    try {
      setState(await commands.savesState());
    } catch (err) {
      onNotice(errText(err), "err");
    }
  }, [onNotice]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const backup = async () => {
    setBusy(true);
    try {
      const record = await commands.savesBackup();
      onNotice(`已备份：${record.message}`);
      await reload();
    } catch (err) {
      onNotice(errText(err), "err");
    } finally {
      setBusy(false);
    }
  };

  const restore = async () => {
    if (!picked) return;
    setBusy(true);
    try {
      const record = await commands.savesRestore(picked);
      onNotice(record.message, "ok");
      setPicked(null);
      setConfirming(false);
      await reload();
    } catch (err) {
      onNotice(errText(err), "err");
      await reload();
    } finally {
      setBusy(false);
    }
  };

  const save = state?.save;
  const frozen = locked(phase);
  const usable = state?.backups.filter((item) => item.usable).length ?? 0;

  return (
    <div className="body">
      <div className="column">
        <section className="card">
          <h2>当前存档</h2>
          <div className="row">
            <span className="chip" data-tone={save?.exists ? "ok" : "busy"}>
              <i />
              {save?.exists ? "库文件在场" : "还没有库文件"}
            </span>
            {save && save.exists ? (
              <>
                <span className="chip">{save.size_text}</span>
                <span className="chip">{save.modified}</span>
              </>
            ) : null}
            <span className="spacer" />
            <button className="btn" data-tone="ghost" disabled={busy} onClick={() => void reload()}>
              重新读取
            </button>
          </div>

          <p className="hint">
            {save?.detail ?? "…"}
          </p>
          {save && !save.exists ? (
            <p className="hint">
              服务端第一次有角色登录时才会建这个库。现在备份不了，也不用备份。
            </p>
          ) : null}

          <div className="kv">
            <span>库文件</span>
            <code>{save?.db_path ? save.db_path : "尚未定下库文件路径"}</code>
            <span>存档目录</span>
            <code>{save?.storage_dir ?? "…"}</code>
            <span>备份目录</span>
            <code>{save?.backup_dir ?? "…"}</code>
          </div>

          {save && save.sidecars.length > 0 ? (
            <p className="hint">随库文件：{save.sidecars.join("、")}</p>
          ) : null}

          <div className="field">
            <div className="row">
              <span className="spacer" />
              <button
                className="btn"
                data-tone="accent"
                disabled={busy || save?.exists !== true}
                onClick={() => void backup()}
              >
                备份一份
              </button>
            </div>
            <p className="hint">
              备份是只读地把整库另写一份到备份目录，服务端在跑也能备；不改原文件，
              也不碰 <code>local.json</code>。
            </p>
          </div>
        </section>
      </div>

      <div className="column">
        <section className="card">
          <h2>已备份的存档</h2>
          <div className="row">
            <span className="chip" data-tone={usable > 0 ? "ok" : undefined}>
              可用 {usable} 份
            </span>
            <span className="chip">共 {state?.backups.length ?? 0} 份</span>
          </div>

          {state && state.backups.length === 0 ? (
            <p className="hint">备份目录还是空的。先在左边点「备份一份」。</p>
          ) : null}

          <div className="save-list">
            {(state?.backups ?? []).map((item: BackupItem) => (
              <button
                key={item.path}
                className="save-item"
                data-on={picked === item.path ? "true" : "false"}
                disabled={!item.usable || busy}
                title={item.usable ? "选它做替换源" : item.note}
                onClick={() => {
                  setPicked(picked === item.path ? null : item.path);
                  setConfirming(false);
                }}
              >
                <b>{item.name}</b>
                <span>
                  {item.modified} · {item.size_text} · 账号 {item.accounts} / 角色 {item.characters}
                </span>
                {item.note ? <em>{item.note}</em> : null}
              </button>
            ))}
          </div>

          {picked ? (
            <div className="field">
              {confirming ? (
                <>
                  <p className="hint" data-tone="err">
                    确认替换？现在这份存档会整库换成选中那份，账号与角色都跟着换。
                    替换前会先把当前那份备份进备份目录，出错也能退回去。
                  </p>
                  <div className="row">
                    <span className="spacer" />
                    <button
                      className="btn"
                      data-tone="ghost"
                      disabled={busy}
                      onClick={() => setConfirming(false)}
                    >
                      取消
                    </button>
                    <button
                      className="btn"
                      data-tone="danger"
                      disabled={busy || frozen}
                      onClick={() => void restore()}
                    >
                      {busy ? "正在替换…" : "确认替换"}
                    </button>
                  </div>
                </>
              ) : (
                <div className="row">
                  <span className="hint">已选：{picked.split(/[\\/]/).pop()}</span>
                  <span className="spacer" />
                  <button
                    className="btn"
                    data-tone="accent"
                    disabled={busy || frozen}
                    onClick={() => setConfirming(true)}
                  >
                    替换为这份
                  </button>
                </div>
              )}
            </div>
          ) : null}

          <p className="hint">
            {frozen
              ? "服务端正在用这份存档，替换要先停止服务端 —— 备份随时可以。"
              : "只能选备份目录里的文件，替换前会验文件头、跑一遍完整性检查、确认三张必备表都在。"}
          </p>
        </section>
      </div>
    </div>
  );
}
