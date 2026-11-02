// fac live driver: opens a terminal with the "fac" profile and types into it
// from <live>/vsc/cmd.json, which tools/live/vscode.sh writes.
//
//   {"send": "<text>" | ["<unit>", ...], "delayMs": n}  sendText, one call per unit
//   {"command": "<id>"}                                run one allowlisted workbench command
//   {"quit": true}                                     dispose the terminal and quit
//
// The live dir is the profile's first argument, so nothing here names a path.
'use strict';

const fs = require('fs');
const path = require('path');
const vscode = require('vscode');

// The only commands {"command": id} may run: they change the terminal's size.
const COMMANDS = new Set([
  'workbench.action.toggleSidebarVisibility',
  'workbench.action.toggleMaximizedPanel',
]);

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function activate(context) {
  const profile = vscode.workspace
    .getConfiguration('terminal.integrated.profiles')
    .get('osx', {}).fac;
  if (!profile || !Array.isArray(profile.args) || profile.args.length < 1) {
    return;
  }
  const dir = path.join(profile.args[0], 'vsc');
  const cmdFile = path.join(dir, 'cmd.json');
  const log = (msg) => fs.appendFileSync(path.join(dir, 'driver.log'), `${new Date().toISOString()} ${msg}\n`);

  const terminal = vscode.window.createTerminal({
    name: 'fac',
    shellPath: profile.path,
    shellArgs: profile.args,
  });
  terminal.show(false);
  terminal.processId.then((pid) => {
    fs.writeFileSync(
      path.join(dir, 'driver.json'),
      JSON.stringify({ ready: true, terminalPid: pid ?? null, extensionHostPid: process.pid }) + '\n',
    );
    log(`terminal open, pid ${pid}`);
  });

  let busy = false;
  const poll = setInterval(async () => {
    if (busy) {
      return;
    }
    let raw;
    try {
      raw = fs.readFileSync(cmdFile, 'utf8');
      fs.unlinkSync(cmdFile);
    } catch {
      return;
    }
    busy = true;
    try {
      const cmd = JSON.parse(raw);
      if (cmd.quit) {
        log('quit');
        clearInterval(poll);
        terminal.dispose();
        await vscode.commands.executeCommand('workbench.action.quit');
        return;
      }
      if (cmd.command !== undefined) {
        if (COMMANDS.has(cmd.command)) {
          await vscode.commands.executeCommand(cmd.command);
          log(`ran ${cmd.command}`);
        } else {
          log(`refused command ${cmd.command}`);
        }
      }
      if (cmd.send !== undefined) {
        const units = Array.isArray(cmd.send) ? cmd.send : [cmd.send];
        const delay = Number(cmd.delayMs) || 0;
        for (let i = 0; i < units.length; i++) {
          terminal.sendText(units[i], false);
          if (delay > 0 && i < units.length - 1) {
            await sleep(delay);
          }
        }
        log(`sent ${units.length} unit(s)`);
      }
    } catch (err) {
      log(`bad command: ${err}`);
    } finally {
      busy = false;
    }
  }, 100);
  context.subscriptions.push({ dispose: () => clearInterval(poll) });
}

function deactivate() {}

module.exports = { activate, deactivate };
