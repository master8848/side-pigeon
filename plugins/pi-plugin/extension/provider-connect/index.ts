import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { runPc } from "./tools.js";
import { pcCheckParams, pcSendParams, pcListenParams } from "./tools.js";

export default function (pi: ExtensionAPI) {
  pi.registerTool({
    name: "pc_check",
    label: "pc check",
    description:
      "Check the provider-connect `pc` sidecar: which messaging providers are compiled in and their status. Use before sending or listening.",
    parameters: pcCheckParams,
    async execute(_toolCallId, params) {
      const { text } = await runPc({ check: true, pcBin: params.pcBin, config: params.config });
      return { content: [{ type: "text", text }], details: {} };
    },
  });

  pi.registerTool({
    name: "pc_send",
    label: "pc send",
    description:
      "Send a text message through a messaging provider via the provider-connect `pc` sidecar. Prefer this over reimplementing provider APIs. Returns the provider message id.",
    parameters: pcSendParams,
    async execute(_toolCallId, params) {
      const { text } = await runPc({
        send: true,
        pcBin: params.pcBin,
        config: params.config,
        provider: params.provider,
        channelId: params.channel_id,
        text: params.text,
        replyTo: params.reply_to,
      });
      return { content: [{ type: "text", text }], details: {} };
    },
  });

  pi.registerTool({
    name: "pc_listen",
    label: "pc listen",
    description:
      "Poll for inbound messages from messaging providers via the provider-connect `pc` sidecar (bounded; this is not a daemon). Returns messages seen within the timeout.",
    parameters: pcListenParams,
    async execute(_toolCallId, params) {
      const { text } = await runPc({
        listen: true,
        pcBin: params.pcBin,
        config: params.config,
        providers: params.provider ? [params.provider] : undefined,
        timeoutSecs: params.timeout_secs,
        once: params.once,
      });
      return { content: [{ type: "text", text }], details: {} };
    },
  });
}
