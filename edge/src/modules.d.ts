// Wrangler `rules` (type "Text", glob "**" + ".sh") imports shell scripts as
// strings — the installer served at /install.sh.
declare module "*.sh" {
  const text: string;
  export default text;
}

// Vite `?raw` imports (unit tests reading the wrangler configs).
declare module "*?raw" {
  const text: string;
  export default text;
}
