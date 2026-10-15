import { useState } from "react";
import { commands, errText, type ConfigView } from "../bridge";

export type LaunchMode = "compat" | "server-only";

/// 上传范围：这是给玩家看的白名单，说一套做一套比什么都不报更糟。
const UPLOADS = [
  "故障发生在哪一步、标题与详情",
  "游戏客户端版本与本机系统环境",
  "启动器版本",
  "退出码（判成崩溃时）",
  "运行日志的尾部，已做大小、编码与隐私过滤",
];

const NEVER = [
  "登录令牌、密码、验证码",
  "完整的用户目录路径与账号信息",
  "整份日志文件",
];

export default function SettingsPage({
  config,
  busy,
  launchMode,
  onLaunchMode,
  onAutoStop,
  onAutoReport,
  onNotice,
}: {
  config: ConfigView | null;
  busy: boolean;
  launchMode: LaunchMode;
  onLaunchMode: (mode: LaunchMode) => void;
  onAutoStop: (enabled: boolean) => Promise<void>;
  onAutoReport: (enabled: boolean) => Promise<void>;
  onNotice: (text: string, tone?: "ok" | "err") => void;
}) {
  const pending = config?.pending_reports ?? 0;
  // 悬停与聚焦共用一个开关：只认鼠标的话，键盘玩家永远看不到这份白名单。
  const [showScope, setShowScope] = useState(false);

  const retry = () => {
    try {
      commands.reportsRetry();
      onNotice("已催一次队列发送，结果稍后写在运行日志里");
    } catch (err) {
      onNotice(errText(err), "err");
    }
  };

  return (
    <div className="body">
      <div className="column">
        <section className="card">
          <h2>启动</h2>

          <div className="field">
            <label>本次启动方式</label>
            <div className="segmented">
              <button
                aria-pressed={launchMode === "compat"}
                disabled={busy}
                onClick={() => onLaunchMode("compat")}
              >
                原生启动（网关 + 客户端）
              </button>
              <button
                aria-pressed={launchMode === "server-only"}
                disabled={busy}
                onClick={() => onLaunchMode("server-only")}
              >
                仅服务端
              </button>
            </div>
            <p className="hint">
              仅服务端不拉起 DFO.exe，用来单独验证网关；停止时同样会把环境收干净。
            </p>
          </div>

          <div className="field">
            <button
              className="switch"
              data-on={config?.auto_stop_on_client_exit ? "true" : "false"}
              disabled={busy}
              onClick={() => void onAutoStop(!config?.auto_stop_on_client_exit)}
            >
              <span>
                关闭游戏后自动停止服务端
                <br />
                <em className="hint">关掉游戏窗口就收尾，不留残留进程</em>
              </span>
              <span className="switch-track" />
            </button>
          </div>
        </section>
      </div>

      <div className="column">
        <section className="card">
          <h2>自动上报</h2>

          <div className="field" data-pop="true" onMouseLeave={() => setShowScope(false)}>
            <button
              className="switch"
              data-on={config?.auto_report ? "true" : "false"}
              disabled={busy}
              onMouseEnter={() => setShowScope(true)}
              onFocus={() => setShowScope(true)}
              onBlur={() => setShowScope(false)}
              onClick={() => void onAutoReport(!config?.auto_report)}
            >
              <span>
                自动上报故障
                <br />
                <em className="hint">关闭时只记在本机，绝不发出去</em>
              </span>
              <span className="switch-track" />
            </button>

            {showScope ? (
              <div className="scope-pop" role="tooltip">
                <div className="scope-col">
                  <b>会上传这些</b>
                  <ul className="scope">
                    {UPLOADS.map((item) => (
                      <li key={item}>{item}</li>
                    ))}
                  </ul>
                </div>
                <div className="scope-col">
                  <b>绝不上传</b>
                  <ul className="scope" data-tone="never">
                    {NEVER.map((item) => (
                      <li key={item}>{item}</li>
                    ))}
                  </ul>
                </div>
              </div>
            ) : null}
          </div>

          <div className="field">
            <div className="row">
              <span className="spacer" />
              <span className="chip" data-tone={pending > 0 ? "busy" : "ok"}>
                待发送 {pending} 条
              </span>
              <button
                className="btn"
                data-tone="accent"
                disabled={busy || pending === 0 || !config?.auto_report}
                onClick={retry}
              >
                立即重试
              </button>
            </div>
            <p className="hint">
              {config?.auto_report
                ? "断网时报告留在本地队列，恢复后按原来那条的编号重发，不会重复计数。"
                : "自动上报未开启：本机记下的故障都留在这台电脑上，随时可以打开开关后发出去。"}
            </p>
          </div>
        </section>
      </div>
    </div>
  );
}
