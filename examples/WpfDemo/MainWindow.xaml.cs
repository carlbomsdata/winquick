using System;
using System.Windows;
using System.Windows.Controls;

namespace DemoApp;

public partial class MainWindow : Window
{
    public MainWindow() => InitializeComponent();

    void Save_Click(object sender, RoutedEventArgs e)
    {
        string name = string.IsNullOrWhiteSpace(NameBox.Text) ? "(nobody)" : NameBox.Text.Trim();
        string dept = (DeptCombo.SelectedItem as ComboBoxItem)?.Content?.ToString() ?? "(none)";
        string mode = AdvancedCheck.IsChecked == true ? "advanced" : "basic";

        StatusText.Text = $"Saved: {name} / {dept} / {mode}";
        HistoryList.Items.Add($"{name} — {dept} — {mode}");
    }
}
