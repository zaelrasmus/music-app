<script lang="ts">
    import type { Snippet } from "svelte";
    import { provideScrollContainer } from "$lib/scroll-container.svelte";

    /**
     * The frame every view sits in.
     *
     * The header never scrolls and the body always does. That is the whole
     * point: the search box, the filters and the view's identity stay put while
     * a thousand-row list moves underneath them.
     *
     * One gutter, `px-8`, shared by the header and the list. The demo used a
     * slightly different left edge on each view and on each element within a
     * view; misalignment like that is felt by people who could never name it,
     * so there is exactly one number here and everything hangs off it.
     */
    interface Props {
        /** Replaces the default title block — used for `ListHeader`. */
        hero?: Snippet;
        title?: string;
        /** A quiet count or state, set beside the title. */
        badge?: string | number | null;
        subtitle?: string;
        /** Sits before the title — a back button, when a view has depth. */
        leading?: Snippet;
        /** Buttons on the title row. */
        actions?: Snippet;
        /** Search boxes and filters — part of the fixed header, not the list. */
        toolbar?: Snippet;
        children: Snippet;
    }

    let {
        hero,
        title,
        badge = null,
        subtitle,
        leading,
        actions,
        toolbar,
        children,
    }: Props = $props();

    /**
     * The element every view scrolls in, offered to anything inside that
     * needs to know what is on screen — which in practice means a virtualised
     * list. Nothing else reads it.
     */
    let scroller = $state<HTMLElement>();
    provideScrollContainer(() => scroller);
</script>

<div class="flex h-full min-h-0 flex-col">
    <header class="flex shrink-0 flex-col gap-4 px-8 pt-6 pb-4">
        {#if hero}
            {@render hero()}
        {:else}
            <div class="flex items-start justify-between gap-4">
                {#if leading}
                    <div class="flex shrink-0 items-center pt-1">
                        {@render leading()}
                    </div>
                {/if}

                <div class="flex min-w-0 flex-1 flex-col gap-1">
                    <h1
                        class="flex items-baseline gap-2.5 text-[28px] leading-[1.15] font-bold tracking-[-0.02em]"
                    >
                        {title}
                        {#if badge !== null && badge !== ""}
                            <span
                                class="text-muted-foreground text-sm font-normal tracking-normal tabular-nums"
                            >
                                {badge}
                            </span>
                        {/if}
                    </h1>
                    {#if subtitle}
                        <p class="text-muted-foreground text-[13px]">{subtitle}</p>
                    {/if}
                </div>

                {#if actions}
                    <div class="flex shrink-0 items-center gap-2">
                        {@render actions()}
                    </div>
                {/if}
            </div>

            {#if toolbar}
                <div class="flex flex-col gap-2">
                    {@render toolbar()}
                </div>
            {/if}
        {/if}
    </header>

    <!-- Rows are indented one step less than the header, so a row's hover
         background bleeds slightly wider than the heading above it and the
         list reads as a surface rather than as a stack of paragraphs. -->
    <div bind:this={scroller} class="min-h-0 flex-1 overflow-y-auto px-5 pb-8">
        {@render children()}
    </div>
</div>
