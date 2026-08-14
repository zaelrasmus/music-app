<script lang="ts">
	import './layout.css';
	import { Toaster } from "$components/ui/sonner/index.js";
	import PlayerBar from "$components/player-bar.svelte";

	const { children } = $props();
</script>

<Toaster />

<!-- Bottom padding keeps the last row clear of the fixed player bar. -->
<div class="pb-28">
	{@render children()}
</div>

<PlayerBar />
