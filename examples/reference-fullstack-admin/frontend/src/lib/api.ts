const API_BASE = 'http://127.0.0.1:3130';

export interface LoginResponse {
	token: string;
	username: string;
}

export interface DashboardSummary {
	active_users: number;
	departments: number;
	total_users: number;
}

export interface Department {
	id: number;
	name: string;
}

export interface UserRecord {
	id: number;
	username: string;
	full_name: string;
	email: string;
	department_id: number;
	department_name: string;
	active: boolean;
}

export interface UserPage {
	items: UserRecord[];
	page: number;
	per_page: number;
	total: number;
}

export async function login(username: string, password: string): Promise<LoginResponse> {
	const response = await fetch(`${API_BASE}/login`, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ username, password })
	});

	if (!response.ok) {
		throw new Error(`Login failed: ${response.status}`);
	}

	return response.json();
}

export async function fetchDashboard(token: string): Promise<DashboardSummary> {
	const response = await fetch(`${API_BASE}/dashboard`, {
		headers: { Authorization: `Bearer ${token}` }
	});

	if (!response.ok) {
		throw new Error(`Dashboard failed: ${response.status}`);
	}

	return response.json();
}

export async function fetchDepartments(token: string): Promise<Department[]> {
	const response = await fetch(`${API_BASE}/departments`, {
		headers: { Authorization: `Bearer ${token}` }
	});

	if (!response.ok) {
		throw new Error(`Departments failed: ${response.status}`);
	}

	return response.json();
}

export async function fetchUsers(token: string): Promise<UserPage> {
	const response = await fetch(`${API_BASE}/users?page=1&per_page=20`, {
		headers: { Authorization: `Bearer ${token}` }
	});

	if (!response.ok) {
		throw new Error(`Users failed: ${response.status}`);
	}

	return response.json();
}
