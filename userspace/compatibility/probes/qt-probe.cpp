// A Qt 6 window for the Linux apps smoke.
//
// Appends JSON lines to $OPENSHELL_APP_PROBE_LOG, or else to
// ~/openshell-app-probe.log: where it runs, every size it takes, and the text
// entered in its field when Enter is pressed. Escape closes it.

#include <QApplication>
#include <QDateTime>
#include <QDir>
#include <QFile>
#include <QJsonDocument>
#include <QJsonObject>
#include <QKeyEvent>
#include <QLineEdit>
#include <QResizeEvent>
#include <QVBoxLayout>
#include <QWidget>

#include <unistd.h>

namespace {

QString LogPath() {
  const QString path = qEnvironmentVariable("OPENSHELL_APP_PROBE_LOG");
  return path.isEmpty() ? QDir::home().filePath("openshell-app-probe.log") : path;
}

void Report(const QString& event, QJsonObject fields = {}) {
  fields["app"] = "qt";
  fields["event"] = event;
  fields["pid"] = static_cast<qint64>(getpid());
  fields["at"] = QDateTime::currentMSecsSinceEpoch() / 1000.0;
  QFile log(LogPath());
  if (log.open(QIODevice::Append | QIODevice::Text)) {
    log.write(QJsonDocument(fields).toJson(QJsonDocument::Compact) + "\n");
  }
}

class Probe final : public QWidget {
 public:
  Probe() {
    setWindowTitle("OpenShell Qt Probe");
    resize(480, 320);
    auto* layout = new QVBoxLayout(this);
    field_ = new QLineEdit(this);
    layout->addWidget(field_);
    connect(field_, &QLineEdit::returnPressed, this,
            [this] { Report("text", {{"value", field_->text()}}); });
  }

  void Focus() { field_->setFocus(); }

 protected:
  void resizeEvent(QResizeEvent* event) override {
    QWidget::resizeEvent(event);
    if (event->size() != last_) {
      last_ = event->size();
      Report("size", {{"width", last_.width()}, {"height", last_.height()}});
    }
  }

  void keyPressEvent(QKeyEvent* event) override {
    if (event->key() == Qt::Key_Escape) {
      close();
      return;
    }
    QWidget::keyPressEvent(event);
  }

 private:
  QLineEdit* field_ = nullptr;
  QSize last_;
};

}  // namespace

int main(int argc, char** argv) {
  QApplication app(argc, argv);
  Probe probe;
  probe.show();
  probe.Focus();
  Report("start", {{"display", qEnvironmentVariable("DISPLAY")},
                   {"xauthority", qEnvironmentVariable("XAUTHORITY")},
                   {"toolkit", "qt6"}});
  const int status = app.exec();
  Report("exit");
  return status;
}
