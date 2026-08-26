using System.Windows.Forms;
static class P { [System.STAThread] static void Main() {
  var f = new Form { Text = "WF48" }; var b = new Button { Name="Go", Text="Go" }; f.Controls.Add(b);
  Application.Run(f); } }
