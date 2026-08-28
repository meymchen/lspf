local server = vim.env.LSPF_MARKDOWN_SERVER or 'lspf-markdown'

vim.lsp.config('lspf_markdown', {
  cmd = { server },
  filetypes = { 'markdown' },
  root_markers = { '.git' },
})
vim.lsp.enable('lspf_markdown')

vim.api.nvim_create_user_command('LspfMarkdownRestart', function()
  vim.lsp.enable('lspf_markdown', false)
  vim.schedule(function()
    vim.lsp.enable('lspf_markdown', true)
    vim.cmd.edit()
  end)
end, {})

vim.api.nvim_create_user_command('LspfMarkdownStop', function()
  vim.lsp.enable('lspf_markdown', false)
end, {})
