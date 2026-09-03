// Tab switching
document.querySelectorAll('.tab-btn').forEach(btn => {
  btn.addEventListener('click', () => {
    const tab = btn.dataset.tab;

    // Hide all sections
    document.getElementById('operators').classList.add('hidden');
    document.getElementById('manage').classList.add('hidden');

    // Show the selected section
    if (tab === 'browse') {
      document.getElementById('operators').classList.remove('hidden');
    } else if (tab === 'manage') {
      document.getElementById('manage').classList.remove('hidden');
    }

    // Update button styles
    document.querySelectorAll('.tab-btn').forEach(b => {
      if (b === btn) {
        b.classList.remove('border-transparent', 'text-gray-500');
        b.classList.add('border-blue-700', 'text-blue-700');
      } else {
        b.classList.remove('border-blue-700', 'text-blue-700');
        b.classList.add('border-transparent', 'text-gray-500');
      }
    });
  });
});
