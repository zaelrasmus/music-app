<script lang="ts">
    import { Button } from "$components/ui/button";
    import { Input } from "$components/ui/input";
    import { prompt } from "$lib/prompt.svelte";

    /**
     * The one modal in the app.
     *
     * Built on the native `<dialog>` rather than a positioned div: `showModal`
     * brings a focus trap, Escape-to-close, inertness for everything behind it
     * and a top-layer stacking context, none of which would be worth
     * reimplementing for a single text field.
     */
    let dialog = $state<HTMLDialogElement | null>(null);
    let input = $state<HTMLInputElement | null>(null);

    $effect(() => {
        if (!dialog) return;

        if (prompt.request && !dialog.open) {
            dialog.showModal();
            // Selected, not just focused: these prompts are usually a rename,
            // where the first keystroke is meant to replace what is there.
            input?.select();
        } else if (!prompt.request && dialog.open) {
            dialog.close();
        }
    });
</script>

<dialog
    bind:this={dialog}
    class="bg-popover text-popover-foreground m-auto w-[min(26rem,calc(100vw-2rem))] rounded-xl border p-0 shadow-2xl backdrop:bg-black/45 backdrop:backdrop-blur-[2px]"
    onclose={() => prompt.cancel()}
    oncancel={(e) => {
        // Let the store settle the promise rather than the browser closing the
        // element out from under it.
        e.preventDefault();
        prompt.cancel();
    }}
>
    {#if prompt.request}
        <form
            class="flex flex-col gap-4 p-5"
            onsubmit={(e) => {
                e.preventDefault();
                prompt.confirm();
            }}
        >
            <div class="flex flex-col gap-1">
                <h2 class="text-base font-semibold">{prompt.request.title}</h2>
                {#if prompt.request.label !== prompt.request.title}
                    <p class="text-muted-foreground text-sm">
                        {prompt.request.label}
                    </p>
                {/if}
            </div>

            <Input
                bind:ref={input}
                bind:value={prompt.value}
                placeholder={prompt.request.placeholder}
                aria-label={prompt.request.label}
            />

            <div class="flex justify-end gap-2">
                <Button type="button" variant="ghost" onclick={() => prompt.cancel()}>
                    Cancel
                </Button>
                <Button type="submit" disabled={prompt.value.trim() === ""}>
                    {prompt.request.confirmLabel}
                </Button>
            </div>
        </form>
    {/if}
</dialog>
