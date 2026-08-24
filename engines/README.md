# Native engine runtime

Native binaries are not stored in Git. Exact upstream URLs, archive hashes,
installation rules, reported versions, and executable hashes are pinned in
`manifest.json`.

Restore the local release inputs before development or packaging:

```powershell
npm run setup:engines
npm run verify:engines
```

Downloaded archives are cached under `.cache/engines/`, and extracted runtimes
are placed in this directory. Both are ignored by Git. The finished NSIS
installer still embeds the complete verified runtimes, so end users do not need
to download or configure engines.
