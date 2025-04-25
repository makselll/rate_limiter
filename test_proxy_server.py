from flask import Flask

app = Flask(__name__)

@app.route('/')
def hello_world():
    return 'Hello, World!'


@app.route('/mama')
def hello_mama():
    return 1 / 0


app.run(debug=True, host='0.0.0.0', port=8000)