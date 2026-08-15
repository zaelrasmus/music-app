/**
 * A modal text prompt.
 *
 * This exists because `window.prompt` does not work here. WebView2 -- the
 * engine Tauri uses on Windows -- has never implemented it, and returns null
 * immediately. The call sites that used it were not erroring; they were
 * silently doing nothing, which is the worst of the available failures.
 *
 * One dialog instance is mounted in the layout and driven from here, so any
 * code that needs a name can `await promptFor(...)` without owning a modal.
 */
export type PromptRequest = {
  title: string;
  label: string;
  placeholder: string;
  initial: string;
  confirmLabel: string;
};

class PromptStore {
  request = $state<PromptRequest | null>(null);
  value = $state("");

  #resolve: ((value: string | null) => void) | null = null;

  open(request: PromptRequest): Promise<string | null> {
    // A second prompt while one is open would strand the first caller's
    // promise forever. Cancelling it is the honest resolution.
    this.#resolve?.(null);

    this.request = request;
    this.value = request.initial;

    return new Promise((resolve) => {
      this.#resolve = resolve;
    });
  }

  /** Empty input is a cancel: a blank name is never what was meant. */
  confirm() {
    const value = this.value.trim();
    this.#finish(value === "" ? null : value);
  }

  cancel() {
    this.#finish(null);
  }

  #finish(value: string | null) {
    const resolve = this.#resolve;
    this.#resolve = null;
    this.request = null;
    this.value = "";
    resolve?.(value);
  }
}

export const prompt = new PromptStore();

/** Resolves to the trimmed text, or null if the user backed out. */
export function promptFor(
  title: string,
  options: Partial<Omit<PromptRequest, "title">> = {},
): Promise<string | null> {
  return prompt.open({
    title,
    label: options.label ?? title,
    placeholder: options.placeholder ?? "",
    initial: options.initial ?? "",
    confirmLabel: options.confirmLabel ?? "Save",
  });
}
