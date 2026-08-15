<script lang="ts">
    import type { Snippet } from "svelte";

    /**
     * The frame every view sits in.
     *
     * The header never scrolls and the body always does. That is the whole
     * point: the search box, the filters and the view's identity stay put while
     * a thousand-row list moves underneath them. Getting this wrong -- one long
     * scrolling column, which is what this app had -- means the controls for a
     * list are only reachable by scrolling back to the top of it.
     */
    interface Props {
        title: string;
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
        title,
        badge = null,
        subtitle,
        leading,
        actions,
        toolbar,
        children,
    }: Props = $props();
</script>

<div class="flex h-full min-h-0 flex-col">
    <header class="flex shrink-0 flex-col gap-3 px-6 pt-5 pb-3">
        <div class="flex items-start justify-between gap-4">
            {#if leading}
                <div class="flex shrink-0 items-center pt-0.5">
                    {@render leading()}
                </div>
            {/if}

            <div class="flex min-w-0 flex-1 flex-col gap-0.5">
                <h1 class="flex items-baseline gap-2 text-[22px] leading-tight font-semibold tracking-[-0.01em]">
                    {title}
                    {#if badge !== null && badge !== ""}
                        <span class="text-muted-foreground text-sm font-normal tabular-nums">
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
    </header>

    <div class="seam mx-6 h-px shrink-0"></div>

    <div class="min-h-0 flex-1 overflow-y-auto px-4 pt-2 pb-8">
        {@render children()}
    </div>
</div>
