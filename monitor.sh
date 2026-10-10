#!/bin/bash

# 检查参数
if [ $# -lt 1 ]; then
    echo "用法: $0 <进程名或PID> [监控间隔(秒)] [输出日志文件]"
    echo "示例: $0 nginx 2 nginx_monitor.log"
    exit 1
fi

TARGET=$1
INTERVAL=${2:-2} # 默认间隔2秒
LOG_FILE=${3:-}

# A bad or zero delay otherwise turns a failed sleep into a hot sampling loop.
# Accept positive decimal seconds, including .5 and 1., before opening a log.
if [[ ! "${INTERVAL}" =~ ^([0-9]+([.][0-9]*)?|[.][0-9]+)$ ]] \
    || [[ -z "${INTERVAL//[0.]/}" ]]; then
    printf '错误: 监控间隔必须是正数秒: %s\n' "${INTERVAL}" >&2
    exit 1
fi

# 确定进程PID
if [[ "$TARGET" =~ ^[0-9]+$ ]]; then
    PID=$TARGET
else
    # 修复点：使用 pgrep -f 查找，但通过 grep -v 排除当前脚本自身的 PID ($$)
    # tail -n 1 取出匹配到的最后一个（通常是较新的）
    PID=$(pgrep -f -- "$TARGET" | grep -x -v "$$" | head -n 1)
fi

# 检查进程是否存在
if [ -z "$PID" ] || ! ps -p "$PID" > /dev/null; then
    echo "错误: 找不到进程 '$TARGET'"
    exit 1
fi

# 获取确切的进程命令名，用于展示
PNAME=$(ps -p "$PID" -o comm=)

echo "开始监控进程: $PNAME (PID: $PID)"
echo "监控间隔: $INTERVAL 秒"
if [ -n "$LOG_FILE" ]; then
    echo "日志将保存至: $LOG_FILE"
fi
echo "按 Ctrl+C 停止监控"
echo "--------------------------------------------------------"

# 打印表头
HEADER=$(printf "%-20s | %-8s | %-8s | %-10s" "时间" "%CPU" "%MEM" "RSS(KB)")
echo "$HEADER"
if [ -n "$LOG_FILE" ]; then
    if ! printf '%s\n' "$HEADER" > "$LOG_FILE"; then
        printf '错误: 无法写入日志: %s\n' "$LOG_FILE" >&2
        exit 1
    fi
fi

# 循环监控
while true; do
    if ! kill -0 "$PID" 2>/dev/null; then
        echo "进程 $PID ($PNAME) 已结束。"
        break
    fi

    CURRENT_TIME=$(date "+%Y-%m-%d %H:%M:%S")

    # 获取数据。进程可能在 kill -0 与 ps 之间退出，因此单独处理这个竞态。
    if ! stats=$(ps -p "$PID" -o %cpu,%mem,rss --no-headers); then
        echo "进程 $PID ($PNAME) 已结束。"
        break
    fi
    read -r cpu mem rss <<< "$stats"

    # 格式化输出
    OUTPUT=$(printf "%-20s | %-8s | %-8s | %-10s" "$CURRENT_TIME" "$cpu" "$mem" "$rss")

    echo "$OUTPUT"

    if [ -n "$LOG_FILE" ]; then
        if ! printf '%s\n' "$OUTPUT" >> "$LOG_FILE"; then
            printf '错误: 无法写入日志: %s\n' "$LOG_FILE" >&2
            exit 1
        fi
    fi

    if ! sleep "$INTERVAL"; then
        printf '错误: 监控等待失败，已停止监控\n' >&2
        exit 1
    fi
done
