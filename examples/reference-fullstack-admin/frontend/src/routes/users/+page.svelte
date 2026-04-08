<script lang="ts">
	import { onMount } from 'svelte';
	import { fetchDepartments, fetchUsers, type Department, type UserPage } from '$lib/api';

	let token = '';
	let users: UserPage | null = null;
	let departments: Department[] = [];
	let error = '';

	onMount(async () => {
		token = sessionStorage.getItem('reference-fullstack-admin-token') ?? '';
		if (!token) {
			error = 'Login first on /login';
			return;
		}

		try {
			users = await fetchUsers(token);
			departments = await fetchDepartments(token);
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		}
	});
</script>

<section class="grid">
	<div class="card">
		<h2>Users</h2>
		{#if error}
			<p class="error">{error}</p>
		{:else if users}
			<p>Total: {users.total}</p>
			<ul>
				{#each users.items as user}
					<li>{user.full_name} · {user.department_name} · {user.email}</li>
				{/each}
			</ul>
		{/if}
	</div>

	<div class="card">
		<h2>Departments</h2>
		<ul>
			{#each departments as department}
				<li>{department.name}</li>
			{/each}
		</ul>
	</div>
</section>

<style>
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
		gap: 1rem;
	}

	.card {
		background: white;
		border: 1px solid #d6dcea;
		border-radius: 18px;
		padding: 1.1rem;
		box-shadow: 0 10px 30px rgba(45, 71, 115, 0.08);
	}

	ul {
		padding-left: 1rem;
	}

	.error {
		color: #8d1f1f;
	}
</style>
