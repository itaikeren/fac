// executeShellCommand, the `exec` Fig hands to custom generators, loadSpec and
// generateSpec functions and getVersionCommand. Evaluating this file as a
// script yields a factory; the spec thread calls it once per context with its
// native `spawn(owner, json)`, which queues the command and returns a request
// id (or -1 when the input is not a command). Neither is a global.
//
// make(owner) -> executeShellCommand for one generator run (`owner` is its
// fetch id); it returns a promise of {stdout, stderr, status}.
// settle(id, json, error) resolves request `id` with the output JSON, or,
// when `error` is given, rejects it with that message.
// drop(owner) forgets the requests of a run that is gone.
//
// The input is Fig's ExecuteCommandInput, {command, args, cwd?, env?,
// timeout?}: as in Fig's executeCommandTimeout, `[input.command,
// ...input.args]` throws (here: rejects) when args is missing. A string is
// also accepted and split on white space into a command and its args; no
// shell runs it.
(function (spawn) {
  const pending = new Map();

  function request(input) {
    if (typeof input === "string") {
      const words = input.trim().split(/\s+/).filter((word) => word !== "");
      return { command: words[0], args: words.slice(1) };
    }
    const args = [...input.args];
    return { command: input.command, args, cwd: input.cwd, env: input.env, timeout: input.timeout };
  }

  return {
    make(owner) {
      return function executeShellCommand(input) {
        return new Promise((resolve, reject) => {
          const id = spawn(owner, JSON.stringify(request(input)));
          if (id < 0) throw new TypeError("executeShellCommand: the input is not a command");
          pending.set(id, { resolve, reject, owner });
        });
      };
    },
    settle(id, json, error) {
      const request = pending.get(id);
      if (request === undefined) return;
      pending.delete(id);
      if (error == null) request.resolve(JSON.parse(json));
      else request.reject(new Error(error));
    },
    drop(owner) {
      for (const [id, request] of pending) {
        if (request.owner === owner) pending.delete(id);
      }
    },
  };
})
