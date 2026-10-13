# vime 流式转写协议 v1

这是参考 OpenAI Realtime transcription 事件命名的**固定子集**，不是完整 OpenAI API 兼容声明。NAS 适配器应按本文实现；换模型时，客户端无需了解模型的原生协议。

每次录音建立独立 WebSocket 连接。客户端使用配置中的完整 URL，不追加路径。生产使用 `wss://` 和系统信任的有效证书；不跳过证书验证。可配置 `api_key_env`，从指定环境变量读取访问令牌，握手时通过 `Authorization: Bearer <token>` 发送。

## 会话与音频

握手后客户端发送：

```json
{
  "type": "session.update",
  "session": {
    "type": "transcription",
    "audio": {
      "input": {
        "format": {"type": "audio/pcm", "rate": 24000},
        "transcription": {"model": "mock-asr"},
        "turn_detection": null
      }
    }
  }
}
```

服务端必须用 `session.updated` 回显 `session.type`、音频格式及 `turn_detection: null`，客户端验证后才上传。关闭服务端 VAD：只有用户停止录音才提交一次完整录音。

音频固定为 **24,000 Hz、单声道、有符号 16 位小端 PCM**，无 WAV 文件头。客户端将本地输入转换成此格式。每个 `append` 为 1–24,000 个采样（最多一秒），JSON WebSocket 消息上限为 1 MiB。

```json
{"type":"input_audio_buffer.append","audio":"<base64 PCM16 LE>"}
```

停止录音时先排空剩余音频，再发送一次：

```json
{"type":"input_audio_buffer.commit"}
```

服务端用以下事件标记这次提交的识别项目：

```json
{"type":"input_audio_buffer.committed","item_id":"recording-1"}
```

## 转写与最终交付

服务端可在录音过程中发送增量：

```json
{"type":"conversation.item.input_audio_transcription.delta","item_id":"recording-1","delta":"你好"}
```

`delta` 是增量，可能为空。客户端没有预览 UI；正常流程以 `completed.transcript` 为最终文本，出错时才尽量恢复已收到的文字。因此模型可修正先前假设，只需在最终事件中提供完整修正结果，无需依赖 partial 拼接。

```json
{"type":"conversation.item.input_audio_transcription.completed","item_id":"recording-1","transcript":"你好，世界。"}
```

`completed` 必须带完整最终文本，且其 `item_id` 必须匹配 commit acknowledgement。客户端处理事件到达顺序，最多交付一次：重复 final 不得重复执行脚本。没有成功提交或确认匹配 item 时，不把 partial 标记为正常 final。错误或超时后，客户端尽量交付已收到的文本，脚本环境变量 `VIME_TRANSCRIPT_STATUS=recovered`；此文本可能不完整。正常交付标记为 `final`。取消不交付，未收到文字时无法恢复。空最终文本不触发输出。

错误使用 `error` 或 `conversation.item.input_audio_transcription.failed`；客户端将其视为失败，并尽量恢复之前收到的文本。服务端错误正文可能包含敏感信息，客户端不直接把正文打印出来。

```json
{"type":"error","error":{"message":"recognition failed"}}
```

## 取消与连接生命周期

取消时客户端发送 `input_audio_buffer.clear` 并关闭连接，不交付结果。关闭连接同样用于隔离已 commit 之后的迟到事件。一次录音不自动重连或重放音频；用户可以重新开始，避免不明确的重复提交和重复动作。

连接和 session acknowledgement 有连接超时；用户停止后等待最终结果有独立超时，均由配置指定。WebSocket 文本事件之外的未知事件不会触发交付；协议格式错误会使本次录音失败。

## Mock 与 TLS

`vime-mock` 是独立进程，使用真实 WebSocket 接收音频并返回预设文本，不做语音识别。可模拟 success、duplicate、error、disconnect、timeout、delayed 和 cancel。

默认本地 mock 使用 `ws://127.0.0.1:8765/v1/realtime`。测试还覆盖 WSS 信任证书握手及不受信任证书拒绝；部署 NAS 时需要有效证书或将私有 CA 正确加入运行账户的系统信任存储。
