import { mountSharedMarkup } from "./markup";

// Imported first by an entry, so the shared markup exists before the entry
// looks up any of its elements.
mountSharedMarkup();
