<script lang="ts">
    import { Input } from "$components/ui/input";
    import SearchIcon from "@lucide/svelte/icons/search";
    import XIcon from "@lucide/svelte/icons/x";

    interface Props {
        value: string;
        placeholder?: string;
        oninput: (value: string) => void;
        onenter?: () => void;
        onclear?: () => void;
        class?: string;
    }

    let {
        value,
        placeholder = "Search…",
        oninput,
        onenter,
        onclear,
        class: className = "",
    }: Props = $props();

    let input = $state<HTMLInputElement | null>(null);

    function clear() {
        onclear ? onclear() : oninput("");
        input?.focus();
    }
</script>

<div class="relative {className}">
    <SearchIcon
        class="text-muted-foreground pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2"
    />
    <Input
        bind:ref={input}
        {value}
        {placeholder}
        class="h-9 pr-9 pl-9"
        oninput={(e) => oninput(e.currentTarget.value)}
        onkeydown={(e) => {
            if (e.key === "Enter") onenter?.();
            // Escape clears rather than blurring: the field is usually the only
            // thing standing between the user and the full list again.
            if (e.key === "Escape" && value !== "") {
                e.stopPropagation();
                clear();
            }
        }}
    />
    {#if value !== ""}
        <button
            type="button"
            class="text-muted-foreground hover:bg-accent hover:text-foreground absolute top-1/2 right-1.5 grid size-6 -translate-y-1/2 place-items-center rounded-md transition-colors"
            aria-label="Clear search"
            onclick={clear}
        >
            <XIcon class="size-3.5" />
        </button>
    {/if}
</div>
