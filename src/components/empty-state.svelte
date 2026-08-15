<script lang="ts">
    import type { Component, Snippet } from "svelte";

    interface Props {
        icon?: Component;
        title: string;
        /** One line saying what to do about it, not just that it is empty. */
        hint?: string;
        children?: Snippet;
    }

    let { icon: Icon, title, hint, children }: Props = $props();
</script>

<div
    class="text-muted-foreground mx-auto flex max-w-sm flex-col items-center gap-3 px-6 py-16 text-center"
>
    {#if Icon}
        <div class="bg-muted/60 grid size-11 place-items-center rounded-xl">
            <Icon class="size-5 opacity-70" />
        </div>
    {/if}
    <div class="flex flex-col gap-1">
        <p class="text-foreground text-sm font-medium">{title}</p>
        {#if hint}
            <p class="text-[13px] leading-relaxed">{hint}</p>
        {/if}
    </div>
    {#if children}
        <div class="mt-1">{@render children()}</div>
    {/if}
</div>
