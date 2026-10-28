{
  find . -type f \( -name "*.tsx" -o -name "*.ts" \) \
  -not -path "*/node_modules/*" \
  -not -path "*/src/components/settings-view/*" \
  -not -path "*/src/components/prompt-manager/*" \
  -not -path "*/src/components/dashboard/*" \
  -not -path "*/src/components/file-explorer/*" \
  -not -path "*/src/components/genre-manager/*" \
  -not -path "*/src/components/music-library/*" \
  -not -path "*/src/components/music-player/*" \
  -not -path "*/src/components/prompt-manager/*" \
  -not -path "*/src/components/genre-manager/*" \
  -not -path "*/src/components/settings-vie/*" \
  -not -path "*/tests/*" \
  -not -path "*/src-tauri/*" \
  -not -path "*/.wrangler/*" \
  -not -path "*/dist/*" \
  -not -path "*/venv/*" \
  -not -path "*/.cache/*" \
  -not -path "*/.git/*" \
  -not -path "*/temp/*" \
  -not -path "*/.venv/*" \
  -not -path "*/.pnpm-store/*" \
  -not -path "*/.astro/*" \
  -not -path "*.d.ts" \
  | sort | while read file; do
    echo "## $file"
    echo
    cat "$file"
    echo
  done
} > output.txt