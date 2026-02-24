# frun

The **frun** is a rust written, lightweight background task manager.  
It is created to replace **screen**, for the problems like buffer lost & wrong emoji display of it.  

## Problems

Some programs like **python** can detact that it is not running in a terminal, so they will take a lazy stdout writting strategy.  
Thus, you may cannot see outputs when you *attach* your session.  
For **python** , you can simply add `-u` arg to solve it. `frun run test_session -- python -u test_script.py`  
Other programs are likely to have a similar solution.  
  
This is **not a bug**, and it **can't be fixed** due to this tool's **different principle** from **screen**.