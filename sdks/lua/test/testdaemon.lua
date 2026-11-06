--- Start a comb daemon for the Lua tests and the conformance runner: one
--- start, one readiness wait.
---
--- The daemon writes a newline on its `--ready-fd` once it serves its socket,
--- and closes it unwritten if it exits first. PUC Lua has no pipe or fd
--- primitives, so a shell does the waiting: the fd is a FIFO in the tempdir,
--- and the shell prints the daemon's pid, then "served" if the newline came
--- within `timeout_s` seconds. bash, for `read -t`.

local M = {}

--- Start `comb_bin daemon` on `socket_path`, with HOME, XDG_CONFIG_HOME and
-- XDG_STATE_HOME pointed into `tmpdir` so its managed defaults and its log
-- never touch the real config or state. Returns its pid once it serves, or
-- nil (the daemon is killed) if it did not within `timeout_s` (default 5).
function M.start(comb_bin, tmpdir, socket_path, timeout_s)
  local start_cmd = "bash -c 'mkfifo \"$1\" || exit 1; "
    .. "XDG_CONFIG_HOME=\"$2\" HOME=\"$2\" XDG_STATE_HOME=\"$2/.local/state\" "
    .. "\"$3\" daemon --socket \"$4\" --ready-fd 3 3>\"$1\" > /dev/null 2>&1 & echo $!; "
    .. "IFS= read -r -t " .. tostring(timeout_s or 5) .. " line < \"$1\" && echo served' _ "
    .. tmpdir .. "/ready " .. tmpdir .. " " .. comb_bin .. " " .. socket_path
  local pipe = io.popen(start_cmd, "r")
  if not pipe then return nil end
  local pid = pipe:read("*l")
  local served = pipe:read("*l")
  pipe:close()
  if served == "served" then return pid end
  if pid then os.execute("kill " .. tostring(pid) .. " 2>/dev/null") end
  return nil
end

return M
