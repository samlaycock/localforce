// Per-browser preferences (the acting user, table columns, SOQL history). Storage can be blocked,
// so every read and write tolerates failure.

const PREFIX = "localforce."

export function stored(key: string): string | null {
  try {
    return localStorage.getItem(PREFIX + key)
  } catch {
    return null
  }
}

export function store(key: string, value: string | null) {
  try {
    if (value === null) localStorage.removeItem(PREFIX + key)
    else localStorage.setItem(PREFIX + key, value)
  } catch {
    // Not kept; the page still works.
  }
}
