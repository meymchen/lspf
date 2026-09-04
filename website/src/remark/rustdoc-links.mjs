const RUSTDOC_PATH = /^lspf::/;

/**
 * Turn rustdoc intra-doc destinations into docs.rs searches for the website.
 * The Markdown remains valid input for `cargo test --doc`, while browser links
 * point at a real HTTP destination.
 */
export default function remarkRustdocLinks() {
  return (tree) => visit(tree);
}

function visit(node) {
  if (node?.type === 'link' && RUSTDOC_PATH.test(node.url)) {
    const item = node.url.replace(RUSTDOC_PATH, '');
    node.url = `https://docs.rs/lspf/latest/lspf/?search=${encodeURIComponent(item)}`;
  }

  // rustdoc accepts comma-separated fence attributes such as `rust,no_run`.
  // Syntax highlighters expect the language name on its own.
  if (node?.type === 'code' && node.lang?.startsWith('rust,')) {
    node.lang = 'rust';
  }

  if (Array.isArray(node?.children)) {
    for (const child of node.children) visit(child);
  }
}
