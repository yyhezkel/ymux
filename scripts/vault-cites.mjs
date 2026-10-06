// Vault cite checker.
//
// A vault page that says "AppState is at lib.rs:146" rots silently the first
// time someone adds a line above it, and the hash gate cannot see it: it only
// proves the page was re-stamped, not that its claims still hold. So a line
// cite carries the symbol it points at — `[sym@file:line](link)` — and this
// module checks identity, not just existence: `sym` must appear within
// CITE_WINDOW lines of `line`. A miss reports where the symbol really is, so
// the fix is copy-paste. A bare `file:line` has no identity to check and is
// rejected outright.
//
// Pure on purpose (text in, errors out; file access injected as readSource)
// so the unit tests need no repo and vault-check owns all the git plumbing.
import { posix } from 'node:path'

export const CITE_WINDOW = 3

const CITE_RE = /\[([A-Za-z_][A-Za-z0-9_]*)@([^\]\s:]+):(\d+)\]\(([^)\s#]+)\)/g
const BARE_RE = /[\w./-]+\.(?:rs|ts|tsx|go|mjs|js|md|yml|yaml|json|toml|ps1|sh):\d+(?:-\d+)?/g
const DEF_KW = '(?:fn|struct|enum|const|static|type|trait|mod|impl|class|function|interface|let|var)'

// Nearest-window miss: definition line first, any mention second, else 0.
function locate(lines, sym) {
  const word = new RegExp(`\\b${sym}\\b`)
  const def = new RegExp(`\\b${DEF_KW}\\s+${sym}\\b`)
  let first = 0
  for (let i = 0; i < lines.length; i++) {
    if (def.test(lines[i])) return i + 1
    if (!first && word.test(lines[i])) first = i + 1
  }
  return first
}

export function checkCites(mdText, mdRel, readSource) {
  const errors = []
  let cites = 0
  const split = new Map() // resolved path → lines | null
  const sourceLines = (p) => {
    if (!split.has(p)) {
      const t = readSource(p)
      split.set(p, t === null ? null : t.split(/\r?\n/))
    }
    return split.get(p)
  }

  mdText.split(/\r?\n/).forEach((text, idx) => {
    const at = `${mdRel}:${idx + 1}: `
    for (const m of text.matchAll(CITE_RE)) {
      cites++
      const [, sym, file, n, link] = m
      const label = `cite ${sym}@${file}:${n}`
      const resolved = posix.normalize(posix.join(posix.dirname(mdRel), link))
      if (file !== resolved && !resolved.endsWith('/' + file)) {
        errors.push(`${at}${label} — cite text '${file}' does not match link target ${resolved}`)
        continue
      }
      const src = sourceLines(resolved)
      if (src === null) {
        errors.push(`${at}${label} — link ${link} resolves to ${resolved}, which git does not track`)
        continue
      }
      const word = new RegExp(`\\b${sym}\\b`)
      const line = Number(n)
      const lo = Math.max(1, line - CITE_WINDOW)
      const hi = Math.min(src.length, line + CITE_WINDOW)
      let hit = false
      for (let i = lo; i <= hi && !hit; i++) hit = word.test(src[i - 1])
      if (hit) continue
      const real = locate(src, sym)
      errors.push(
        real
          ? `${at}${label} — '${sym}' is not within ±${CITE_WINDOW} of ${resolved}:${n}; found at ${resolved}:${real} → ${sym}@${file}:${real}`
          : `${at}${label} — '${sym}' not found in ${resolved} (renamed or deleted? fix or drop the cite)`,
      )
    }
    // mask valid cites so their own file:line is not read as bare
    const rest = text.replace(CITE_RE, (s) => ' '.repeat(s.length))
    for (const b of rest.matchAll(BARE_RE)) {
      errors.push(`${at}bare file:line cite '${b[0]}' — write it as [symbol@${b[0]}](link)`)
    }
  })
  return { cites, errors }
}
