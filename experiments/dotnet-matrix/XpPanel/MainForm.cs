using System;
using System.Windows.Forms;

namespace XpPanel
{
    public class MainForm : Form
    {
        private readonly TextBox _name = new TextBox();
        private readonly Button _greet = new Button();
        private readonly Label _status = new Label();

        public MainForm()
        {
            Text = "XP Panel";
            Width = 380;
            Height = 200;

            _name.Name = "NameBox";
            _name.SetBounds(20, 40, 240, 22);

            _greet.Name = "GreetButton";
            _greet.Text = "Greet";
            _greet.SetBounds(20, 75, 100, 28);
            _greet.Click += Greet_Click;

            _status.Name = "StatusLabel";
            _status.Text = "Idle";
            _status.SetBounds(20, 115, 320, 22);

            Label prompt = new Label();
            prompt.Text = "Name";
            prompt.SetBounds(20, 18, 100, 18);

            Controls.Add(prompt);
            Controls.Add(_name);
            Controls.Add(_greet);
            Controls.Add(_status);
        }

        private void Greet_Click(object sender, EventArgs e)
        {
            _status.Text = "Hello, " + _name.Text + "!";
        }
    }
}
