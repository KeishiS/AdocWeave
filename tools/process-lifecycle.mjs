export function hasExited(child) {
  return child.exitCode !== null || child.signalCode !== null;
}

export function waitForExit(child, milliseconds) {
  if (hasExited(child)) return Promise.resolve(true);
  return new Promise((resolveWait) => {
    const exited = () => {
      clearTimeout(timer);
      resolveWait(true);
    };
    const timer = setTimeout(() => {
      child.off("exit", exited);
      resolveWait(false);
    }, milliseconds);
    child.once("exit", exited);
  });
}
