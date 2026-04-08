<script lang="ts">
	import { onMount } from 'svelte';
	import { fetchDashboard, type DashboardSummary } from '$lib/api';

	let token = '';
	let summary: DashboardSummary | null = null;
	let error = '';

	onMount(async () => {
		token = sessionStorage.getItem('reference-fullstack-admin-token') ?? '';
		if (!token) {
			error = 'Login first on /login';
			return;
		}

		try {
			summary = await fetchDashboard(token);
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		}
	});
</script>

<section class="grid">
	<div class="card intro">
		<h2>Public-Only Reference App</h2>
		<p>This page shows the smallest useful fullstack admin surface built for public reference.</p>
	</div>

	{#if error}
		<div class="card error">{error}</div>
	{:else if summary}
		<div class="stats">
			<div class="card stat">
				<span>Active Users</span>
				<strong>{summary.active_users}</strong>
			</div>
			<div class="card stat">
				<span>Departments</span>
				<strong>{summary.departments}</strong>
			</div>
			<div class="card stat">
				<span>Total Users</span>
				<strong>{summary.total_users}</strong>
			</div>
		</div>
	{/if}
</section>

<style>
	.grid, .stats {
		display: grid;
		gap: 1rem;
	}

	.stats {
		grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
	}

	.card {
		background: white;
		border: 1px solid #d6dcea;
		border-radius: 18px;
		padding: 1.1rem;
		box-shadow: 0 10px 30px rgba(45, 71, 115, 0.08);
	}

	.stat span {
		display: block;
		color: #60708d;
		margin-bottom: 0.4rem;
	}

	.stat strong {
		font-size: 2rem;
	}

	.error {
		color: #8d1f1f;
	}
</style>
