<script lang="ts">
	import "./layout.css";
	import { onMount } from "svelte";
	import { ModeWatcher } from "mode-watcher";
	import { Toaster } from "$components/ui/sonner/index.js";
	import Titlebar from "$components/titlebar.svelte";
	import AppSidebar from "$components/app-sidebar.svelte";
	import PlayerBar from "$components/player-bar.svelte";
	import QueuePanel from "$components/queue-panel.svelte";
	import PromptDialog from "$components/prompt-dialog.svelte";
	import { chrome } from "$lib/chrome.svelte";
	import { sidebar } from "$lib/sidebar.svelte";
	import { queueStore } from "$lib/queue.svelte";
	import { player } from "$lib/player.svelte";

	const { children } = $props();


	/**
	 * The window is configured `visible: false`, so showing it is this app's
	 * job. Done here rather than in a view because the layout is the last thing
	 * that can fail before something is on screen -- and a window that never
	 * appears is indistinguishable from an app that never started.
	 */
	onMount(() => {
		const chromeReady = chrome.start();
		void sidebar.restore();

		return () => {
			chromeReady.then((stop) => stop());
		};
	});

	/**
	 * Shortcuts.
	 *
	 * Deliberately few, and none of them single letters: this app has text
	 * inputs on every view, and a bare `s` for search would fight every one of
	 * them. Anything typed into a field is left alone.
	 */
	function shortcut(event: KeyboardEvent) {
		const target = event.target as HTMLElement | null;
		const typing =
			target?.tagName === "INPUT" ||
			target?.tagName === "TEXTAREA" ||
			target?.isContentEditable === true;

		if (event.ctrlKey && !event.shiftKey && !event.altKey && event.key === "b") {
			event.preventDefault();
			sidebar.cycle();
			return;
		}

		if (event.ctrlKey && !event.shiftKey && !event.altKey && event.key === "q") {
			event.preventDefault();
			queueStore.toggle();
			return;
		}

		if (typing) return;

		// Space is the one unmodified key worth taking. It is what every player
		// uses, and the check above means it still types a space in a search box.
		if (event.key === " ") {
			event.preventDefault();
			void player.togglePlayPause();
		}
	}
</script>

<svelte:window onkeydown={shortcut} />

<!-- `track` keeps following the OS until the user pins a mode; the dark class
     goes on <html>, which is what `@custom-variant dark` matches. -->
<ModeWatcher />

<Toaster position="bottom-center" />
<PromptDialog />

<!--
  The shell.

  `h-screen` with every region a flex child, so nothing is positioned
  absolutely and nothing needs bottom padding to clear the player bar. The
  window itself never scrolls -- the one scroll container is inside whichever
  view is open.
-->
<div class="flex h-screen flex-col overflow-hidden">
	<Titlebar />

	<div class="flex min-h-0 flex-1 overflow-hidden">
		<AppSidebar />

		<main class="min-w-0 flex-1 overflow-hidden">
			{@render children()}
		</main>

		<QueuePanel />
	</div>

	<PlayerBar />
</div>
