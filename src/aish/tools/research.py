import os
import re
import subprocess
from datetime import datetime, timedelta

from aish.i18n import t
from aish.tools.base import ToolBase


class SmartLogTool(ToolBase):
    """智能日志分析工具，根据用户的诊断需求自动推断日志来源并返回匹配摘要，而不仅仅是输出原始日志。"""

    def __init__(self):
        super().__init__(
            name="smart_log",
            description=t("tools.smart_log.description"),
            parameters={
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": t("tools.smart_log.param.query"),
                    },
                    "path": {
                        "type": "string",
                        "description": t("tools.smart_log.param.path"),
                    },
                },
                "required": ["query"],
            },
        )

    def __call__(self, query: str, path: str = "") -> str:
        """根据用户查询分析日志并返回摘要，而不仅仅是输出原始日志。"""
        # 1. 如果未显式给定 path，根据 query 关键字自动推断
        query_lc = query.lower()
        sources = []  # (source_type, identifier)  source_type: "file", "dir", "systemd"
        patterns = []  # regex patterns we care about

        if path:
            # 调用者显式指定了日志来源
            if os.path.exists(path):
                if os.path.isdir(path):
                    sources.append(("dir", path))
                else:
                    sources.append(("file", path))
            else:
                sources.append(("systemd", path))
        else:
            # 推断
            if "nginx" in query_lc:
                sources.append(("dir", "/var/log/nginx"))
                sources.append(("systemd", "nginx"))
                patterns = [r"\berror\b", r"\bwarn\b", r"\bcrit\b"]
            elif "登录" in query or "login" in query_lc:
                sources.append(("file", "/var/log/auth.log"))
                sources.append(("file", "/var/log/secure"))
                sources.append(("systemd", "sshd"))
                patterns = [r"Failed password", r"Accepted password", r"Invalid user"]
            elif "网速" in query or "network" in query_lc:
                sources.append(("systemd", "NetworkManager"))
                patterns = [r"link is (down|up)", r"error", r"WARN"]
            else:
                # 默认查看系统日志
                sources.append(("systemd", "-b"))  # 当前 boot 全部日志
                patterns = [r"error", r"fail", r"crit"]

        # 2. 收集并分析日志
        summary_lines = []
        since = (datetime.now() - timedelta(days=1)).strftime("%Y-%m-%d %H:%M:%S")

        def _analyze_text(text: str) -> dict:
            """返回匹配行数、样例行等"""
            matches = []
            total = 0
            for line in text.splitlines():
                total += 1
                for pat in patterns:
                    if re.search(pat, line, re.IGNORECASE):
                        matches.append(line)
                        break
            sample = "\n".join(matches[:10])
            return {"total": total, "matched": len(matches), "sample": sample}

        for stype, ident in sources:
            try:
                if stype == "file":
                    if os.path.exists(ident):
                        raw = subprocess.check_output(
                            ["tail", "-n", "1000", ident],
                            text=True,
                            stderr=subprocess.STDOUT,
                        )
                        res = _analyze_text(raw)
                        summary_lines.append(
                            t(
                                "tools.smart_log.summary.file",
                                ident=ident,
                                matched=res["matched"],
                                total=res["total"],
                                sample=res["sample"],
                            )
                        )
                elif stype == "dir":
                    for root, _, files in os.walk(ident):
                        for f in files:
                            if f.endswith(".log"):
                                fpath = os.path.join(root, f)
                                raw = subprocess.check_output(
                                    ["tail", "-n", "1000", fpath],
                                    text=True,
                                    stderr=subprocess.STDOUT,
                                )
                                res = _analyze_text(raw)
                                summary_lines.append(
                                    t(
                                        "tools.smart_log.summary.dir",
                                        path=fpath,
                                        matched=res["matched"],
                                        total=res["total"],
                                        sample=res["sample"],
                                    )
                                )
                elif stype == "systemd":
                    cmd = (
                        ["journalctl", "-u", ident, "--since", since, "--no-pager"]
                        if ident != "-b"
                        else ["journalctl", "-b", "--since", since, "--no-pager"]
                    )
                    raw = subprocess.check_output(
                        cmd, text=True, stderr=subprocess.STDOUT
                    )
                    res = _analyze_text(raw)
                    summary_lines.append(
                        t(
                            "tools.smart_log.summary.systemd",
                            ident=ident,
                            matched=res["matched"],
                            total=res["total"],
                            sample=res["sample"],
                        )
                    )
            except subprocess.CalledProcessError as e:
                summary_lines.append(
                    t(
                        "tools.smart_log.summary.call_failed",
                        stype=stype.upper(),
                        ident=ident,
                        output=e.output,
                    )
                )

        if not summary_lines:
            return t("tools.smart_log.no_logs_found")

        return "\n\n".join(summary_lines)
