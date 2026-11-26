// langspan-demo-scenes-v1: fictional, deterministic scenes for
// `langspan dashboard --demo`. They use the coordinator's view format so the
// live renderer draws them, and the page labels them as simulated. They are
// not coordinator data and not a measured result. The live page never
// contains this file.
const DEMO_SCENES = (() => {
  const repo = 'repository:demo-repo';
  const ids = {
    upload: 'claude-code.6f1c2a90-upload',
    caller: 'codex.9b7e4d21-caller',
    retry: 'codex.3c8a5f10-retry',
    tests: 'claude-code.d42e7b63-tests',
  };
  const local = (id, workspace, branchName, task, editing, recent, state) => ({
    id: 'local:' + id, group: repo, local: true, machine: null, host: id.split('.')[0],
    session: id.split('.')[1].slice(0, 8), session_id: id, workspace_id: workspace,
    branch_ref: 'refs/heads/' + branchName, task, intent: null, editing,
    recent: recent.map(([path, age]) => ({ path, age_ms: age })), more_files: 0,
    state: state || (editing.length ? 'editing' : 'active'), liveness: null, active_age_ms: 4000, heard_age_ms: null,
  });
  const peer = (participant, name, host, session, branchName, task, editing, recent, heard) => ({
    id: 'peer:' + participant + ':' + host + ':' + session, group: repo, local: false, machine: name, host, session,
    session_id: null, workspace_id: 'shop', branch_ref: 'refs/heads/' + branchName, task, intent: null, editing,
    recent: recent.map((path) => ({ path, age_ms: null })), more_files: 0, state: editing.length ? 'editing' : 'published',
    liveness: null, active_age_ms: null, heard_age_ms: heard,
  });
  const overlap = (path, members, editing) => ({
    workspace_id: 'shop', path, group: repo, editing, more_agents: 0,
    agents: members.map((m) => m[1]), members: members.map((m) => m[0]),
  });
  const workspaces = [
    { workspace_id: 'shop', group: repo, root: '/example/shop', branch_ref: 'refs/heads/main', sessions: 2,
      scan_failure: null, teammates: 2, coverage: { scanned: true, indexable_files: 214, current_files: 214, partial_files: 0 },
      publishing: { remote: 'origin', name: 'Demo laptop', enabled: true, withdrawn: false, share_task: true, sequence: 41 } },
    { workspace_id: 'shop-retry', group: repo, root: '/example/shop-retry', branch_ref: 'refs/heads/feat/retry', sessions: 2,
      scan_failure: null, teammates: 0, coverage: { scanned: true, indexable_files: 214, current_files: 212, partial_files: 2 },
      publishing: null },
  ];
  const scene = (n) => {
    const notice = n >= 1, refused = n >= 2, landed = n >= 3;
    const agents = [
      local(ids.upload, 'shop', 'main', 'Add resumable uploads to the API', ['src/api.rs'], [['src/upload.rs', 9000]]),
      local(ids.caller, 'shop', 'main', 'Call the upload API from the CLI', [], [['src/cli.rs', 15000], ['src/api.rs', 40000]]),
      local(ids.retry, 'shop-retry', 'feat/retry', 'Retry failed uploads', landed ? [] : ['src/retry.rs'], landed ? [['src/retry.rs', 20000]] : [], landed ? 'active' : undefined),
      local(ids.tests, 'shop-retry', 'feat/retry', 'Integration tests for retries', [], [['tests/upload.rs', 12000], ...(refused ? [['src/retry.rs', 30000]] : [])]),
      peer('0a1b2c3d4e5f60718293a4b5c6d7e8f9', 'Rowan', 'codex', 'c0ffee00', 'feat/resume', 'Resume interrupted uploads', [], ['src/upload.rs', 'src/api.rs'], 18000),
      peer('f9e8d7c6b5a4938271605f4e3d2c1b0a', 'Sam', 'claude-code', 'ab12cd34', 'docs/upload', null, [], ['docs/upload.md'], 65000),
    ];
    const label = (id) => {
      const row = agents.find((a) => a.id === id);
      return row.local ? row.host + ' ' + row.session : row.machine + ': ' + row.host + ' ' + row.session;
    };
    const member = (id) => [id, label(id)];
    const peerRowan = agents[4].id;
    const overlaps = [
      overlap('src/api.rs', [member(peerRowan), member('local:' + ids.caller), member('local:' + ids.upload)].sort((a, b) => a[1].localeCompare(b[1])), true),
      overlap('src/upload.rs', [member(peerRowan), member('local:' + ids.upload)].sort((a, b) => a[1].localeCompare(b[1])), false),
    ];
    if (refused) overlaps.push(overlap('src/retry.rs', [member('local:' + ids.retry), member('local:' + ids.tests)].sort((a, b) => a[1].localeCompare(b[1])), !landed));
    const checks = [];
    if (refused) checks.push({ workspace_id: 'shop-retry', name: 'integration', result: { type: 'failed', exit_code: 101 },
      tree_oid: '8a21bd5c884e0f3f2a9d1b7c6e5d4a3b2c1d0e9f', combined: true, applies: landed ? 'stale' : 'unknown', duration_ms: 7800, age_ms: landed ? 61000 : 7000 });
    if (landed) checks.unshift({ workspace_id: 'shop-retry', name: 'integration', result: { type: 'passed' },
      tree_oid: 'f73c90b2d8281a4e5b6c7d8e9f0a1b2c3d4e5f60', combined: true, applies: 'unknown', duration_ms: 8400, age_ms: 3000 });
    return {
      generation: 7, uptime_ms: 5400000, active_agents: 6, teammates_heard: 2, teammates_known: 2,
      agents, more_agents: 0,
      editing: [
        { workspace_id: 'shop', path: 'src/api.rs', session_id: ids.upload, expires_in_ms: 27000, base_unknown: false },
        ...(landed ? [] : [{ workspace_id: 'shop-retry', path: 'src/retry.rs', session_id: ids.retry, expires_in_ms: 21000, base_unknown: false }]),
      ],
      more_editing: 0, overlaps, more_overlaps: 0,
      interface_changes: notice ? [{ workspace_id: 'shop', path: 'src/api.rs', by_session: ids.upload,
        changes: [{ name: 'upload', kind: 'function', change: 'signature_changed' }], more_changes: 0,
        dependents: ['src/cli.rs', 'src/upload.rs'], more_dependents: 0, age_ms: 24000 }] : [],
      more_interface_changes: 0,
      denials: { conflicts: refused ? [{ session_id: ids.tests, paths: ['src/retry.rs'], more_paths: 0, holders: [ids.retry], age_ms: null }] : [],
        more_conflicts: false, stale_refusals: [], more_stale_refusals: false, window: 5000 },
      checks, more_checks: 0,
      landings: landed ? [{ workspace_id: 'shop-retry', onto: 'main', commit_oid: '83b41c907c29d5e6f7a8b9c0d1e2f3a4b5c6d7e8',
        tree_oid: 'f73c90b2d8281a4e5b6c7d8e9f0a1b2c3d4e5f60', check_id: 'check-0042', inputs: 2 }] : [],
      more_landings: 0, push_turns: [], more_push_turns: 0,
      messages: notice ? [{ workspace_id: 'shop', from: ids.upload, to: { kind: 'session', session_id: ids.caller },
        text: 'upload() now takes a resume token; update the CLI call before you add the flag.', paths: ['src/api.rs'], acknowledged: refused ? 1 : 0 }] : [],
      more_messages: 0,
      notes: [{ workspace_id: 'shop', author: null, text: 'Keep the upload API backward compatible until the next release.', paths: ['src/api.rs'] }],
      more_notes: 0, workspaces, more_workspaces: 0, uninstalled_hosts: [],
    };
  };
  return [
    { caption: 'Four local sessions in two worktrees and two teammates. Two files have recent shared activity.', dashboard: scene(0) },
    { caption: 'An interface changes in src/api.rs; the view names the files that use it, and a message is waiting.', dashboard: scene(1) },
    { caption: 'A session in the linked worktree is refused an edit of a held file; a combined check fails.', dashboard: scene(2) },
    { caption: 'The hold is released, the combined check passes on a new tree, and the landing is recorded.', dashboard: scene(3) },
  ];
})();
