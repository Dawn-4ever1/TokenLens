import { isTauri } from "./api";

/** Copy user-requested text using the OS clipboard inside the desktop shell. */
export async function copyText(text: string): Promise<void> {
  if (isTauri()) {
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    await writeText(text);
    return;
  }
  if (navigator.clipboard) await navigator.clipboard.writeText(text);
}
