---
"localforce": minor
---

The web UI is rebuilt as a React app with shadcn/ui components. It works as before, with a few changes:

- Each view has its own address under `/__localforce/ui/`, such as `/__localforce/ui/data/Account/<id>` for a record and `/__localforce/ui/log/<n>` for a logged request. Old `#/data/…` links still work.
- A table's search, recycle bin, sort and page are kept in the URL, so a filtered list can be shared or bookmarked.
- The objects list, the record editor and the API log's detail can be resized, and there's a light, dark and system theme switch.
- `LOCALFORCE_UI_DIR` is gone: debug builds read the built UI from disk, and `bun run dev` in `ui/` serves it with hot reload.
