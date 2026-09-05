// Illustrative server traces, not captured telemetry. Ownership and ordering follow
// engine.rs::dispatch, service.rs::build_service_stack, and session.rs::claim_cancellation.
export type TraceOwner = 'wire' | 'protocol' | 'user';
export interface TraceEvent {
  owner: TraceOwner;
  title: string;
  route: string;
  payload: string;
  note: string;
}
export interface ProtocolTrace {
  label: string;
  method: string;
  context: string;
  events: TraceEvent[];
}

export function protocolTraces(zh: boolean): ProtocolTrace[] {
  const t = (en: string, cn: string) => zh ? cn : en;
  const json = (value: unknown) => JSON.stringify(value, null, 2);
  const uri = 'file:///workspace/main.rs';
  const hoverResult = { contents: { kind: 'plaintext', value: 'fn main()' } };
  return [
    {
      label: t('Hover request', '悬停请求'),
      method: 'textDocument/hover',
      context: t('Initialized connection · a hover handler and a user Layer are registered.', '连接已初始化，已注册 hover 处理器和一个用户 Layer。'),
      events: [
        {
          owner: 'wire', title: 'textDocument/hover', route: 'IDE → Server',
          payload: json({ jsonrpc: '2.0', id: 41, method: 'textDocument/hover', params: { textDocument: { uri }, position: { line: 0, character: 3 } } }),
          note: t('A request has an ID. The Transport adapter delivers the JSON-RPC envelope.', '请求携带 ID。Transport 适配器交付 JSON-RPC 消息封装。'),
        },
        {
          owner: 'protocol', title: t('Admission & fixed protections', '准入与固定保护'), route: 'ProtocolEngine → Service',
          payload: 'request #41: admitted\nparams: decoded\nstack: panic isolation → tracing → concurrency limit',
          note: t('Inbound capacity is reserved before decoding parameters or creating the handler task.', '先预留入站容量，再解码参数和创建处理任务。'),
        },
        {
          owner: 'user', title: t('Registered user Layer', '已注册的用户 Layer'), route: 'Layer → next',
          payload: 'method: textDocument/hover\naction: forward the decoded call',
          note: t('This example Layer forwards the call. The last registered user Layer runs outermost.', '示例中的 Layer 将调用继续向内传递。最后注册的用户 Layer 位于最外侧用户位置。'),
        },
        {
          owner: 'protocol', title: t('Frozen Router selects the handler', '冻结的 Router 选择处理器'), route: 'RouterService → hover',
          payload: 'route: textDocument/hover\ninput: Arc<State> + ServerContext + HoverParams + CancellationToken',
          note: t('The Router is framework-owned; the selected handler is application-owned.', 'Router 由框架拥有，选中的处理器由应用提供。'),
        },
        {
          owner: 'user', title: t('Handler returns hover content', '处理器返回悬停内容'), route: 'hover → result',
          payload: json(hoverResult),
          note: t('Illustrative handler output. Language analysis belongs to your application.', '示例处理器的返回内容。语言分析由你的应用实现。'),
        },
        {
          owner: 'protocol', title: t('Encode & enqueue', '编码并入队'), route: 'Protocol session → outbound queue',
          payload: 'response #41: encoded\nbudgets: message count + encoded bytes\nwriter: Transport',
          note: t('The connection accounts for the response until the transport attempt finishes.', '连接统计响应的消息数与字节占用，直到传输尝试完成。'),
        },
        {
          owner: 'wire', title: t('Response #41', '响应 #41'), route: 'Server → IDE',
          payload: json({ jsonrpc: '2.0', id: 41, result: hoverResult }),
          note: t('The response carries the original request ID, with a result and no method field.', '响应携带原请求 ID 和 result，不包含 method 字段。'),
        },
      ],
    },
    {
      label: t('Document sync', '文档同步'),
      method: 'textDocument/didChange',
      context: t('Initialized connection · main.rs is open at version 1 · a change hook is registered.', '连接已初始化，main.rs 已打开且版本为 1，已注册变更钩子。'),
      events: [
        {
          owner: 'wire', title: 'textDocument/didChange', route: 'IDE → Server',
          payload: json({ jsonrpc: '2.0', method: 'textDocument/didChange', params: { textDocument: { uri, version: 2 }, contentChanges: [{ text: 'fn main() {}\n' }] } }),
          note: t('A full-content change notification: no request ID and no response.', '一次完整内容变更通知：没有请求 ID，也不产生响应。'),
        },
        {
          owner: 'protocol', title: t('Validate & apply the change', '校验并应用变更'), route: 'ProtocolEngine → Documents',
          payload: 'document: file:///workspace/main.rs\nversion: 1 → 2\ntext: "fn main() {}\\n"',
          note: t('Mutation happens in the read loop, outside user Layers and before the hook.', '状态变更在读取循环中执行，位于用户 Layer 之外，先于用户钩子。'),
        },
        {
          owner: 'protocol', title: t('Dispatch the post-mutation hook', '分发变更后钩子'), route: 'Service stack → Router',
          payload: 'stack: panic isolation → tracing → concurrency limit\nthen: Router → registered hook\nnotification: textDocument/didChange',
          note: t('The built-in mutation is already complete when user dispatch starts.', '用户分发开始时，协议内建的状态变更已经完成。'),
        },
        {
          owner: 'user', title: t('Hook observes the new snapshot', '钩子读取新快照'), route: 'didChange hook → ServerContext',
          payload: 'ctx.documents(): read-only DocumentsView\nobserved version: 2\nJSON-RPC response: none',
          note: t('The registered hook observes updated state. Diagnostics would be a separate notification sent by application code.', '已注册的钩子观察更新后的状态。若要发布诊断，应由应用另行发送通知。'),
        },
      ],
    },
    {
      label: t('Cancellation', '请求取消'),
      method: '$/cancelRequest',
      context: t('Request #41 is still in flight when cancellation arrives. No new user dispatch occurs.', '取消到达时，请求 #41 仍在处理中；不会产生新的用户分发。'),
      events: [
        {
          owner: 'wire', title: '$/cancelRequest', route: 'IDE → Server',
          payload: json({ jsonrpc: '2.0', method: '$/cancelRequest', params: { id: 41 } }),
          note: t('params.id identifies the request to cancel. This notification has no top-level id.', 'params.id 标识要取消的请求。取消通知本身没有顶层 id。'),
        },
        {
          owner: 'protocol', title: t('Claim & signal cancellation', '认领请求并触发取消'), route: 'ProtocolEngine → Protocol session',
          payload: 'inbound registry: remove request #41\nCancellationToken: signalled\nuser Layers / Router: bypassed',
          note: t('The existing handler is cancelled cooperatively. Unknown or completed IDs are ignored.', '通过取消信号协作式停止已有处理任务。未知或已完成的请求 ID 被忽略。'),
        },
        {
          owner: 'protocol', title: t('Complete the original request', '结束原请求'), route: 'Protocol session → outbound queue',
          payload: 'request #41: RequestCancelled\nerror code: -32800\nresponse: encoded and enqueued',
          note: t('This completes the original request, not the cancellation notification. Blocking tasks and side effects are not rolled back.', '这里结束的是原请求，不是对取消通知作出响应。阻塞任务与副作用不会因此回滚。'),
        },
        {
          owner: 'wire', title: t('Error response #41', '错误响应 #41'), route: 'Server → IDE',
          payload: json({ jsonrpc: '2.0', id: 41, error: { code: -32800, message: 'request cancelled' } }),
          note: t('Cancellation wins in this example. If the result completed first, there would be no second response.', '本例中取消先完成。若结果已先完成，则不会产生第二个响应。'),
        },
      ],
    },
  ];
}
